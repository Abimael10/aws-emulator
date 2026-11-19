// SPDX-License-Identifier: CC0-1.0

//! The IAM ROLE an Amazon ECS service is created with decides whether its LOAD-BALANCER
//! BINDING can be made at all — and so whether a CloudFormation stack that declares such a
//! service completes or fails; and a binding that WAS validated leaves the service's running
//! task a registered target of the group it bound.
//!
//! RULE: `CreateService$role` is "the name or full Amazon Resource Name (ARN) of the IAM role
//! that allows Amazon ECS to make calls to your load balancer on your behalf", so a create
//! that hands a role over has its `loadBalancers` binding validated UNDER that role: Amazon
//! ECS assumes it and performs the Elastic Load Balancing actions of a registration with it —
//! reading the target group back, which is what validates the binding, and registering a
//! target into it. A role the policies in force do not let perform one of those on the group
//! the entry names is a binding Amazon ECS could not validate, so the create is refused with
//! `InvalidParameterException` and the sentence AWS words that refusal with, the service is NOT
//! created, and a stack whose `AWS::ECS::Service` declares it fails and rolls back instead of
//! completing over a service nothing created. A create that hands over no role takes the Amazon
//! ECS service-linked role, which carries those permissions, so nothing of the account's
//! decides it.
//!
//! And a binding the role ADMITS is really made: once the scheduler has placed the task its
//! desired count asks for, that task is a target of the group the binding named — reached at
//! the EC2 INSTANCE that hosts it, on the HOST port its container's port mapping was bound to,
//! because a task that is not in the `awsvpc` network mode "is associated with ... an Amazon
//! EC2 instance" rather than with an interface of its own
//! (`LoadBalancer$targetGroupArn`).
//!
//! Five crates have to agree for that to hold, which is why this suite lives here: IAM holds
//! the role and the documents in force on it, Elastic Load Balancing v2 owns the target group,
//! the actions a registration performs on it and the registrations themselves, Amazon EC2 holds
//! the network and the instance the capacity comes from, Amazon ECS owns the create and the
//! task, and CloudFormation owns the stack that fails or completes over it. Everything is
//! driven through the real operations on one engine, so what is asserted is what each service
//! persisted.
//!
//! The rows differ along what the rule generalises over: WHICH statements the role carries —
//! every action of the service on every resource (the grant AWS's own service-role policy
//! makes), the two actions named explicitly on the group the binding names, only the
//! registration without the read that validates the group, every action but on ANOTHER group,
//! nothing of the service at all, an allow beside an explicit deny — and a service that hands
//! over no role at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output reader this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions the query-wire services below publish.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";
const EC2_API_VERSION: &str = "2016-11-15";
const ELBV2_API_VERSION: &str = "2015-12-01";
const IAM_API_VERSION: &str = "2010-05-08";

/// The `X-Amz-Target` prefix of the one awsJson service this suite drives.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// `InvalidParameterException$message` Amazon ECS refuses a binding it could not validate
/// with: fixed text naming the member and the service role whose permissions the validation is
/// made under, and nothing of the value the request carried.
///
/// Read from the Amazon ECS crate, which owns the sentence, so the suite and the service cannot
/// word it differently.
const UNVALIDATED: &str =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::UNVALIDATED_TARGET_GROUP_MESSAGE;

/// The error code that refusal carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The stack statuses a create is still in flight in, the one it finishes in, and the one a
/// failed create's rollback finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up or its rollback finishing, the capacity its instance's agent registers, the task the
/// scheduler then places and the registration that task earns — and how often it looks again.
/// Several times the span those moves take: the deadline is only here so a failure is a failure
/// rather than a hang, and every wait looks for its CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// The cluster, the task-definition family and the container every row below runs, and the
/// names of the load balancer and the target group a binding names.
const CLUSTER: &str = "ecsrolebinding";
const FAMILY: &str = "ecsrolebinding";
const CONTAINER: &str = "app";
const TARGET_GROUP: &str = "ecsrolebinding-tg";
const LOAD_BALANCER: &str = "ecsrolebinding-alb";

/// A second target group, which a row's grant names INSTEAD of the one its binding does.
const OTHER_TARGET_GROUP: &str = "ecsrolebinding-other";

/// The statements one row's role carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No role at all: the create takes the Amazon ECS service-linked role.
    NoRole,
    /// Every action of Elastic Load Balancing v2 on every resource — the grant AWS's own
    /// `AmazonEC2ContainerServiceRole` policy makes.
    EveryElbAction,
    /// The two actions a registration performs, named on the group the binding names.
    BothOnTheGroup,
    /// The registration alone: the read that validates the group is missing.
    RegisterOnly,
    /// Every action, but named on the OTHER group.
    EveryActionElsewhere,
    /// Another service's actions only.
    NothingOfElb,
    /// Every action allowed, beside an explicit deny of the registration.
    AllowBesideDeny,
}

/// One row: the grant its role carries, whether the binding can be validated, and the names
/// its own resources take so two rows never collide in the services that hold them.
struct Row {
    case: &'static str,
    grant: Grant,
    /// Whether the binding can be validated, so the create succeeds and the stack completes.
    admitted: bool,
    stack: &'static str,
    role: &'static str,
    service: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the service-role grant: every action of the service on every resource",
        grant: Grant::EveryElbAction,
        admitted: true,
        stack: "ecsrole-every",
        role: "ecsRoleEvery",
        service: "ecsroleevery",
    },
    Row {
        case: "no role at all: the service-linked role",
        grant: Grant::NoRole,
        admitted: true,
        stack: "ecsrole-none",
        role: "ecsRoleNone",
        service: "ecsrolenone",
    },
    Row {
        case: "both actions, named on the group the binding names",
        grant: Grant::BothOnTheGroup,
        admitted: true,
        stack: "ecsrole-both",
        role: "ecsRoleBoth",
        service: "ecsroleboth",
    },
    Row {
        case: "the registration alone: the read that validates the group is missing",
        grant: Grant::RegisterOnly,
        admitted: false,
        stack: "ecsrole-register",
        role: "ecsRoleRegister",
        service: "ecsroleregister",
    },
    Row {
        case: "every action, but named on another group",
        grant: Grant::EveryActionElsewhere,
        admitted: false,
        stack: "ecsrole-elsewhere",
        role: "ecsRoleElsewhere",
        service: "ecsroleelsewhere",
    },
    Row {
        case: "nothing of Elastic Load Balancing at all",
        grant: Grant::NothingOfElb,
        admitted: false,
        stack: "ecsrole-nothing",
        role: "ecsRoleNothing",
        service: "ecsrolenothing",
    },
    Row {
        case: "an allow beside an explicit deny of the registration",
        grant: Grant::AllowBesideDeny,
        admitted: false,
        stack: "ecsrole-denied",
        role: "ecsRoleDenied",
        service: "ecsroledenied",
    },
];

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them — the CLASSIC Elastic Load Balancing front door
/// included, since the two Elastic Load Balancing models share one endpoint and one signing
/// name there.
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
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation of a service whose model declares a form-encoded query wire, answered
/// with its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
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
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One query-wire call that must succeed, answered with its XML body.
async fn ok_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    let (status, body) = query(router, signing_name, version, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {signing_name} {action}, got {status} with body {body}"
    );
    body
}

/// One CloudFormation call that must succeed.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    ok_query(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        action,
        members,
        case,
    )
    .await
}

/// One IAM call that must succeed.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    ok_query(router, "iam", IAM_API_VERSION, action, members, case).await
}

/// One Elastic Load Balancing v2 call that must succeed.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    ok_query(
        router,
        "elasticloadbalancingv2",
        ELBV2_API_VERSION,
        action,
        members,
        case,
    )
    .await
}

/// Call one Amazon ECS operation, answered with its status and parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: Value) -> (u16, Value) {
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
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Amazon ECS call that must succeed.
async fn ok_ecs(router: &MultiServiceRouter, operation: &str, request: Value, case: &str) -> Value {
    let (status, parsed) = ecs(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The trust policy every role below carries: the principal Amazon ECS assumes a service's
/// role as, which is what lets it be assumed at all.
fn ecs_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The inline policy one row's role carries, written against the two target-group ARNs.
fn policy_document(grant: Grant, group: &str, other: &str) -> Option<String> {
    let statements = match grant {
        Grant::NoRole => return None,
        Grant::EveryElbAction => json!([
            { "Effect": "Allow", "Action": "elasticloadbalancing:*", "Resource": "*" },
        ]),
        Grant::BothOnTheGroup => json!([{
            "Effect": "Allow",
            "Action": [
                "elasticloadbalancing:DescribeTargetGroups",
                "elasticloadbalancing:RegisterTargets",
            ],
            "Resource": group,
        }]),
        Grant::RegisterOnly => json!([{
            "Effect": "Allow",
            "Action": "elasticloadbalancing:RegisterTargets",
            "Resource": "*",
        }]),
        Grant::EveryActionElsewhere => json!([{
            "Effect": "Allow",
            "Action": "elasticloadbalancing:*",
            "Resource": other,
        }]),
        Grant::NothingOfElb => json!([{
            "Effect": "Allow",
            "Action": "ec2:Describe*",
            "Resource": "*",
        }]),
        Grant::AllowBesideDeny => json!([
            { "Effect": "Allow", "Action": "elasticloadbalancing:*", "Resource": "*" },
            {
                "Effect": "Deny",
                "Action": "elasticloadbalancing:RegisterTargets",
                "Resource": "*",
            },
        ]),
    };
    Some(json!({ "Version": "2012-10-17", "Statement": statements }).to_string())
}

/// Create one row's role through IAM's own API, with the statements its grant describes, and
/// answer the ARN IAM minted — or `None` for a row that hands over no role.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    grant: Grant,
    group: &str,
    other: &str,
) -> Option<String> {
    let document = policy_document(grant, group, other)?;
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role),
            ("AssumeRolePolicyDocument", &ecs_trust()),
        ],
        case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "binding"),
            ("PolicyDocument", &document),
        ],
        case,
    )
    .await;
    Some(element(&created, "Arn"))
}

/// The default subnets of the account's own default VPC, which nothing here created: the
/// network a deployment that declares none of its own places its load balancer and its
/// instances in.
async fn default_subnets(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let subnets = elements(
        &ok_query(
            router,
            "ec2",
            EC2_API_VERSION,
            "DescribeSubnets",
            &[
                ("Filter.1.Name", "default-for-az"),
                ("Filter.1.Value.1", "true"),
            ],
            case,
        )
        .await,
        "subnetId",
    );
    assert!(
        subnets.len() >= 2,
        "case {case:?}: the account's default VPC must hold a default subnet per zone, got \
         {subnets:?}"
    );
    subnets
}

/// The load balancer, the two target groups and the listener every row's binding is made
/// against, plus the cluster and the revision its tasks run — created once, through each
/// owning service's own API, and answered as the group a binding names and the one a grant may
/// name instead.
async fn given_estate(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let subnets = default_subnets(router, case).await;
    let vpc = element(
        &ok_query(
            router,
            "ec2",
            EC2_API_VERSION,
            "DescribeVpcs",
            &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
            case,
        )
        .await,
        "vpcId",
    );
    let balancer = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", LOAD_BALANCER),
            ("Subnets.member.1", &subnets[0]),
            ("Subnets.member.2", &subnets[1]),
        ],
        case,
    )
    .await;
    let balancer_arn = element(&balancer, "LoadBalancerArn");
    let mut groups = Vec::new();
    for name in [TARGET_GROUP, OTHER_TARGET_GROUP] {
        let created = elbv2(
            router,
            "CreateTargetGroup",
            &[
                ("Name", name),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("TargetType", "instance"),
                ("VpcId", &vpc),
            ],
            case,
        )
        .await;
        groups.push(element(&created, "TargetGroupArn"));
    }
    // Only a listener can send traffic to a group, which is what lets a service bind it.
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", &balancer_arn),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", &groups[0]),
        ],
        case,
    )
    .await;
    ok_ecs(
        router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    ok_ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "bridge",
            "requiresCompatibilities": ["EC2"],
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": "public.ecr.aws/amazonlinux/amazonlinux:2",
                "essential": true,
                "cpu": 128,
                "memory": 256,
                "command": ["sh", "-c", "echo bound; sleep 3600"],
                "portMappings": [{ "containerPort": 80, "protocol": "tcp" }],
            }],
        }),
        case,
    )
    .await;
    (groups[0].clone(), groups[1].clone())
}

/// The `loadBalancers` entry every row's service binds.
fn binding(target_group_arn: &str) -> Value {
    json!([{
        "targetGroupArn": target_group_arn,
        "containerName": CONTAINER,
        "containerPort": 80,
    }])
}

/// The template one row deploys: the cluster and revision it reuses, and the service that
/// binds the group under the role the row created.
fn template(row: &Row, role_arn: Option<&str>, target_group_arn: &str) -> String {
    let role = match role_arn {
        Some(arn) => format!("\n      Role: {arn}"),
        None => String::new(),
    };
    format!(
        r#"Resources:
  Service:
    Type: AWS::ECS::Service
    Properties:
      ServiceName: {service}
      Cluster: {CLUSTER}
      TaskDefinition: {FAMILY}
      DesiredCount: 1
      LaunchType: EC2{role}
      LoadBalancers:
        - ContainerName: {CONTAINER}
          ContainerPort: 80
          TargetGroupArn: {target_group_arn}
Outputs:
  ServiceOut:
    Value: !GetAtt Service.Name
"#,
        service = row.service,
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED:
/// looked at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, "DescribeStacks", &[("StackName", stack)], case).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        let in_flight = reported == CREATE_IN_PROGRESS || reported == ROLLBACK_IN_PROGRESS;
        if !in_flight || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every `ResourceStatusReason` one stack's event stream carries, which is where the refusal a
/// failing resource earned is reported.
async fn stack_failures(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let body = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)], case).await;
    elements(&body, "ResourceStatusReason")
}

/// Whether Amazon ECS holds a service of one row's name in the shared cluster.
async fn service_is_held(router: &MultiServiceRouter, case: &str, service: &str) -> bool {
    let described = ok_ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [service] }),
        case,
    )
    .await;
    described["services"]
        .as_array()
        .is_some_and(|services| !services.is_empty())
}

#[tokio::test]
async fn an_ecs_services_binding_is_validated_under_the_role_the_create_hands_over() {
    let router = build_router();
    let (group, other) = given_estate(&router, "the shared estate").await;

    for row in ROWS {
        let case = row.case;
        let role_arn = given_role(&router, case, row.role, row.grant, &group, &other).await;

        // Amazon ECS's OWN refusal first: the create is what the role decides, and what it
        // answers is the code and the sentence AWS words this refusal with.
        let direct = format!("{}direct", row.service);
        let mut request = json!({
            "cluster": CLUSTER,
            "serviceName": direct,
            "taskDefinition": FAMILY,
            "desiredCount": 1,
            "launchType": "EC2",
            "loadBalancers": binding(&group),
        });
        if let Some(arn) = role_arn.as_deref() {
            request["role"] = Value::String(arn.to_string());
        }
        let (status, answered) = ecs(&router, "CreateService", request).await;
        if row.admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be admitted, got {status} with {answered}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the create to be refused, got {status} with {answered}"
            );
            assert!(
                answered["__type"]
                    .as_str()
                    .is_some_and(|kind| kind.ends_with(INVALID_PARAMETER)),
                "case {case:?}: expected {INVALID_PARAMETER}, got {answered}"
            );
            assert_eq!(
                answered["message"].as_str(),
                Some(UNVALIDATED),
                "case {case:?}: expected the sentence a binding that could not be validated \
                 earns, got {answered}"
            );
            assert!(
                !service_is_held(&router, case, &direct).await,
                "case {case:?}: a refused create must leave no service behind"
            );
        }

        // ...and a stack declaring that same service reaches the terminal status the refusal
        // decides: a create that was refused created nothing, so the stack rolls back.
        let body = template(row, role_arn.as_deref(), &group);
        cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack), ("TemplateBody", &body)],
            case,
        )
        .await;
        let described = settled_stack(&router, case, row.stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        let expected = if row.admitted {
            CREATE_COMPLETE
        } else {
            ROLLBACK_COMPLETE
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stack to end in {expected}, got {reported:?}; the \
             reasons it recorded were {:?}",
            stack_failures(&router, case, row.stack).await,
        );
        if row.admitted {
            let outputs = outputs_of(&described);
            assert_eq!(
                outputs.get("ServiceOut").map(String::as_str),
                Some(row.service),
                "case {case:?}: the stack must report the service it created, got {outputs:?}"
            );
            assert!(
                service_is_held(&router, case, row.service).await,
                "case {case:?}: Amazon ECS must hold the service the stack created"
            );
        } else {
            assert!(
                stack_failures(&router, case, row.stack)
                    .await
                    .iter()
                    .any(|reason| reason.contains(UNVALIDATED)),
                "case {case:?}: the stack must report the refusal its resource earned, got {:?}",
                stack_failures(&router, case, row.stack).await
            );
            assert!(
                !service_is_held(&router, case, row.service).await,
                "case {case:?}: a rolled-back stack must leave no service behind"
            );
        }
    }
}

/// A service whose binding the role ADMITS really converges: the scheduler places the task its
/// desired count asks for on the capacity the cluster holds, and that task becomes a registered
/// target of the group the binding named — at the EC2 instance that hosts it, on the host port
/// its container was bound to.
#[tokio::test]
async fn an_admitted_binding_leaves_the_services_task_registered_in_its_group() {
    let router = build_router();
    let case = "a binding the role admits";
    let (group, other) = given_estate(&router, case).await;
    let subnet = default_subnets(&router, case).await[0].clone();

    // The capacity the EC2 launch type runs on: an instance of the ECS-optimized image whose
    // user data joins this cluster, which is how the container agent on it registers.
    let stack = "ecsrole-capacity";
    let capacity = format!(
        r#"Resources:
  Instance:
    Type: AWS::EC2::Instance
    Properties:
      ImageId: '{{{{resolve:ssm:/aws/service/ecs/optimized-ami/amazon-linux-2/recommended/image_id}}}}'
      InstanceType: t3.micro
      SubnetId: {subnet}
      UserData:
        Fn::Base64: |
          #!/bin/bash
          echo ECS_CLUSTER={CLUSTER} >> /etc/ecs/ecs.config
Outputs:
  InstanceOut:
    Value: !Ref Instance
"#
    );
    cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &capacity)],
        case,
    )
    .await;
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the capacity stack must complete; the reasons it recorded were {:?}",
        stack_failures(&router, case, stack).await,
    );
    let instance = outputs_of(&described)
        .get("InstanceOut")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the capacity stack must report its instance"));

    let service = "ecsroleconverges";
    let role_arn = given_role(
        &router,
        case,
        "ecsRoleConverges",
        Grant::EveryElbAction,
        &group,
        &other,
    )
    .await
    .unwrap_or_else(|| panic!("case {case:?}: the row's role must be created"));
    ok_ecs(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": service,
            "taskDefinition": FAMILY,
            "desiredCount": 1,
            "launchType": "EC2",
            "role": role_arn,
            "loadBalancers": binding(&group),
        }),
        case,
    )
    .await;

    // The scheduler places the task the count asks for...
    let deadline = Instant::now() + DEADLINE;
    let mut held;
    loop {
        held = ok_ecs(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [service] }),
            case,
        )
        .await;
        if held["services"][0]["runningCount"].as_u64() == Some(1) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        held["services"][0]["runningCount"].as_u64(),
        Some(1),
        "case {case:?}: the service must run the task its count asks for, got {held}"
    );

    // ...and that task is a registered target of the group the binding named, at the instance
    // that hosts it and on the HOST port its container was bound to.
    let deadline = Instant::now() + DEADLINE;
    let mut registered;
    loop {
        let health = elbv2(
            &router,
            "DescribeTargetHealth",
            &[("TargetGroupArn", &group)],
            case,
        )
        .await;
        registered = elements(&health, "Id");
        if !registered.is_empty() || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        registered,
        vec![instance.clone()],
        "case {case:?}: expected the instance that hosts the task to be the group's target"
    );

    // The port the registration carries is the HOST port the task reports for its container,
    // read back out of Amazon ECS itself — never the container port the binding named.
    let listed = ok_ecs(
        &router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": service }),
        case,
    )
    .await;
    let task_arns: Vec<&str> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let tasks = ok_ecs(
        &router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": task_arns }),
        case,
    )
    .await;
    let host_port = tasks["tasks"][0]["containers"][0]["networkBindings"][0]["hostPort"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the task's container must report its host binding, got {tasks}")
        });
    let health = elbv2(
        &router,
        "DescribeTargetHealth",
        &[("TargetGroupArn", &group)],
        case,
    )
    .await;
    assert_eq!(
        elements(&health, "Port"),
        vec![host_port.to_string()],
        "case {case:?}: expected the registration at the host port the task reports, got {health}"
    );
}
