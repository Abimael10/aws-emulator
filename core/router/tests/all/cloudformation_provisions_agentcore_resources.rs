// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` has really created the
//! `AWS::BedrockAgentCore::*` resources its template declares — in the Amazon Bedrock
//! AgentCore control plane that owns them.
//!
//! The rule under test: **a stack creates every resource its template declares by calling
//! the OWNING service's own create API, so the identifier the stack publishes for a
//! resource resolves in that service; and a resource type the stack provisions is one
//! template validation accepts.** An AgentCore resource is addressed by the identifier that
//! control plane MINTED for it, so the id a deployed stack publishes has to be one
//! `GetAgentRuntime` / `GetMemory` / `GetGateway` / `GetGatewayTarget` resolves, rather than
//! the control plane answering `ResourceNotFoundException` for a resource its own stack just
//! reported as created. Two crates have to agree for that to hold (CloudFormation makes the
//! call, the AgentCore control plane serves it), so the suite lives here: a crate-local test
//! can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: the resource FAMILY (an agent
//! runtime, a memory, a gateway and one of its targets), the template spelling (JSON with
//! long-form intrinsics, YAML with the short-form tags), how the output reads the resource
//! back (`Ref`, each `Fn::GetAtt` attribute), whether the identifier's label is the declared
//! name as written or lower-cased, whether the declaration names an execution role the same
//! stack creates in IAM or one it states literally, and whether the resource is a top-level
//! one or a CHILD that can only be created once its parent is ready.
//!
//! The update half and the deletion half are the same rule: an update applies a changed
//! declaration through that service's own update API — which is what publishes a runtime's
//! next version — and deleting the stack deletes the resources it created.
//!
//! Nothing here waits a fixed time for a stack: a deployment's own transition and the
//! control plane's are both clock-owed, so every wait polls the condition it is waiting for
//! until it holds or a generous deadline passes ([`settled_stack`], [`poll_until`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery percent-encoder and the XML readers every awsQuery suite in this crate
// shares, written once in `all/query_wire.rs` so no suite re-derives the encoding or the
// reading its assertions depend on.
use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack and its
/// AgentCore resources belong to.
const REGION: &str = "us-east-1";

/// How long a clock-owed transition is waited for before the wait is a failure: generously
/// more than the spans the stack and the control plane owe, so a loaded host still reaches
/// the condition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The signing name the AgentCore control plane shares with the AgentCore data plane
/// (`sigv4Name: bedrock-agentcore`): the route decides which of the two serves a request.
const AGENTCORE: &str = "bedrock-agentcore";

/// The service principal an execution role this control plane assumes has to admit.
const AGENTCORE_PRINCIPAL: &str = "bedrock-agentcore.amazonaws.com";

/// The container image an agent runtime's artifact names. `ContainerConfiguration.containerUri`
/// admits an Amazon ECR registry or the public one, which is what this is.
const CONTAINER_URI: &str = "public.ecr.aws/stack-deployed/agent:1";

/// The ARN namespace the control plane stamps on its resources
/// (`awsService.arnNamespace: bedrock-agentcore`), in the Region this suite signs for.
fn agentcore_arn_prefix() -> String {
    format!("arn:aws:bedrock-agentcore:{REGION}:")
}

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them: CloudFormation, the AgentCore control plane the
/// stack calls, and IAM — because the control plane resolves an agent runtime's execution
/// role against this account's IAM before it stores the runtime.
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
        service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
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

/// Call one AgentCore control-plane operation over its own restJson1 wire, on the method and
/// URI its `@http` binding names, under the signing name both AgentCore planes share.
async fn agentcore(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|document| document.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header(
            "host",
            format!("bedrock-agentcore-control.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization(AGENTCORE))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The error code an AgentCore refusal carries, read off the `__type` its restJson1 error
/// document names the shape in.
fn error_code(answer: &Value) -> Option<&str> {
    answer["__type"].as_str()?.rsplit('#').next()
}

/// Look again until `condition` holds or [`SETTLE_DEADLINE`] passes, panicking with `case`
/// and whatever the last look saw.
///
/// Every wait in this suite goes through here: what is being waited for is a CONDITION the
/// clock makes true, never an elapsed time, so nothing in this file sleeps for a span and
/// then asserts.
async fn poll_until<F, Fut>(case: &str, waiting_for: &str, mut look: F) -> String
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = (bool, String)>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (held, seen) = look().await;
        if held {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!("case {case:?}: timed out waiting for {waiting_for}; last saw {seen}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a stack status is one a deployment STOPS in, rather than one it is passing
/// through: CloudFormation's own statuses end in `_IN_PROGRESS` while work is outstanding.
fn is_terminal(stack_status: &str) -> bool {
    stack_status.ends_with("_COMPLETE") || stack_status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack rests in a terminal status, and answer that body.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    poll_until(case, "the stack to rest in a terminal status", || async {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        if status != 200 {
            return (false, format!("HTTP {status} from DescribeStacks: {body}"));
        }
        let settled = opt_element(&body, "StackStatus").is_some_and(|it| is_terminal(&it));
        (settled, body)
    })
    .await
}

/// Create a stack, acknowledging the IAM resources a template may declare, then wait for it
/// to settle and answer the `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            // "If you have IAM resources with custom names, you must specify
            // CAPABILITY_NAMED_IAM", which acknowledges both forms.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// One output the stack must report, as the text it carries.
fn published(case: &str, body: &str, key: &str) -> String {
    outputs_of(body)
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
        .clone()
}

/// The IAM role an agent runtime's declaration names, as the resources a template declares:
/// the control plane resolves the role before it stores the runtime, so the role the stack
/// publishes has to be one this account's IAM really holds, with a trust policy admitting
/// this service.
fn agent_role(role_name: &str) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": role_name,
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": AGENTCORE_PRINCIPAL },
                    "Action": "sts:AssumeRole"
                }]
            }
        }
    })
}

#[tokio::test]
async fn a_declared_agent_runtime_resolves_through_the_control_plane() {
    let router = build_router();
    let case = "a runtime deployed in the JSON spelling with its own IAM role";
    let stack_name = "agentcore-runtime-stack";
    let template = json!({
        "Resources": {
            "AgentRole": agent_role("stack-agent-runtime-role"),
            "AgentRuntime": {
                "Type": "AWS::BedrockAgentCore::Runtime",
                "Properties": {
                    "AgentRuntimeName": "stackRuntimeOne",
                    "Description": "deployed by a CloudFormation stack",
                    "RoleArn": {"Fn::GetAtt": ["AgentRole", "Arn"]},
                    "AgentRuntimeArtifact": {
                        "ContainerConfiguration": {"ContainerUri": CONTAINER_URI}
                    },
                    "NetworkConfiguration": {"NetworkMode": "PUBLIC"}
                }
            }
        },
        "Outputs": {
            "RuntimeId": {"Value": {"Fn::GetAtt": ["AgentRuntime", "AgentRuntimeId"]}},
            "RuntimeArn": {"Value": {"Fn::GetAtt": ["AgentRuntime", "AgentRuntimeArn"]}},
            "RuntimeRef": {"Value": {"Ref": "AgentRuntime"}},
            "RuntimeVersion": {
                "Value": {"Fn::GetAtt": ["AgentRuntime", "AgentRuntimeVersion"]}
            },
            "RoleArn": {"Value": {"Fn::GetAtt": ["AgentRole", "Arn"]}}
        }
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body}"
    );

    let runtime_id = published(case, &body, "RuntimeId");
    assert_eq!(
        published(case, &body, "RuntimeRef"),
        runtime_id,
        "case {case:?}: the type's primary identifier is its id, so `Ref` must answer it, \
         got {body}"
    );
    assert_eq!(
        published(case, &body, "RuntimeVersion"),
        "1",
        "case {case:?}: a created runtime carries its first version, got {body}"
    );

    // The control plane itself resolves the id the stack published, and what it holds is
    // what the template declared.
    let (status, reported) = get_runtime(&router, &runtime_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to resolve the id the stack published \
         ({runtime_id}), got {status} with {reported}"
    );
    assert_eq!(
        reported["agentRuntimeName"].as_str(),
        Some("stackRuntimeOne"),
        "case {case:?}: expected the runtime to carry the declared name, got {reported}"
    );
    assert_eq!(
        reported["description"].as_str(),
        Some("deployed by a CloudFormation stack"),
        "case {case:?}: expected the declared description, got {reported}"
    );
    assert_eq!(
        reported["roleArn"].as_str(),
        Some(published(case, &body, "RoleArn").as_str()),
        "case {case:?}: expected the runtime to run as the role the stack created, got \
         {reported}"
    );
    assert_eq!(
        reported["agentRuntimeArtifact"]["containerConfiguration"]["containerUri"].as_str(),
        Some(CONTAINER_URI),
        "case {case:?}: expected the declared container artifact, got {reported}"
    );
    assert_eq!(
        reported["agentRuntimeArn"].as_str(),
        Some(published(case, &body, "RuntimeArn").as_str()),
        "case {case:?}: expected the published ARN to be the one the control plane minted, \
         got {reported}"
    );
    let arn = reported["agentRuntimeArn"].as_str().unwrap_or_default();
    assert!(
        arn.starts_with(&agentcore_arn_prefix()) && arn.ends_with(&runtime_id),
        "case {case:?}: expected an ARN in the AgentCore namespace naming the runtime, got \
         {arn:?}"
    );

    // A stack whose create reported the runtime created reported it only once that service
    // had brought it up, so the status a client reads straight after is the settled one.
    assert_eq!(
        reported["status"].as_str(),
        Some("READY"),
        "case {case:?}: expected a completed stack to have waited for the runtime to be \
         ready, got {reported}"
    );
}

#[tokio::test]
async fn an_update_publishes_the_next_version_of_the_runtime_the_stack_still_declares() {
    let router = build_router();
    let case = "an update that changes the runtime's artifact and description";
    let stack_name = "agentcore-runtime-updated";
    let declaration = |description: &str, tag: &str| {
        json!({
            "Resources": {
                "AgentRole": agent_role("stack-updated-runtime-role"),
                "AgentRuntime": {
                    "Type": "AWS::BedrockAgentCore::Runtime",
                    "Properties": {
                        "AgentRuntimeName": "stackRuntimeTwo",
                        "Description": description,
                        "RoleArn": {"Fn::GetAtt": ["AgentRole", "Arn"]},
                        "AgentRuntimeArtifact": {
                            "ContainerConfiguration": {
                                "ContainerUri": format!("public.ecr.aws/stack-deployed/agent:{tag}")
                            }
                        },
                        "NetworkConfiguration": {"NetworkMode": "PUBLIC"}
                    }
                }
            },
            "Outputs": {
                "RuntimeId": {"Value": {"Fn::GetAtt": ["AgentRuntime", "AgentRuntimeId"]}},
                "RuntimeVersion": {
                    "Value": {"Fn::GetAtt": ["AgentRuntime", "AgentRuntimeVersion"]}
                }
            }
        })
        .to_string()
    };

    let body = create_and_settle(&router, case, stack_name, &declaration("before", "1")).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body}"
    );
    let runtime_id = published(case, &body, "RuntimeId");

    let after = declaration("after", "2");
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", after.as_str()),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let body = settled_stack(&router, case, stack_name).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {body}"
    );

    // The runtime the new template still declares is the SAME one, carrying the changed
    // configuration — and the control plane published its NEXT version for the change.
    let (status, reported) = get_runtime(&router, &runtime_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to still hold {runtime_id}, got {status} \
         with {reported}"
    );
    assert_eq!(
        reported["description"].as_str(),
        Some("after"),
        "case {case:?}: expected the changed description to reach the runtime, got {reported}"
    );
    assert_eq!(
        reported["agentRuntimeArtifact"]["containerConfiguration"]["containerUri"].as_str(),
        Some("public.ecr.aws/stack-deployed/agent:2"),
        "case {case:?}: expected the changed artifact to reach the runtime, got {reported}"
    );
    let version = published(case, &body, "RuntimeVersion");
    assert_eq!(
        version, "2",
        "case {case:?}: expected the update to publish the runtime's next version, got \
         {version:?} in {body}"
    );
    assert_eq!(
        reported["agentRuntimeVersion"].as_str(),
        Some(version.as_str()),
        "case {case:?}: expected the control plane to report the version the stack \
         published, got {reported}"
    );
}

#[tokio::test]
async fn a_declared_memory_resolves_through_the_control_plane() {
    let router = build_router();
    let case = "a memory deployed in the YAML spelling with the short-form tags";
    let stack_name = "agentcore-memory-stack";
    // The memory family's `name` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$` and its
    // `eventExpiryDuration` is `@range(min: 3, max: 365)` days.
    let template = "\
Resources:
  Memory:
    Type: AWS::BedrockAgentCore::Memory
    Properties:
      Name: stackMemoryOne
      Description: deployed by a CloudFormation stack
      EventExpiryDuration: 45
Outputs:
  MemoryId:
    Value: !GetAtt Memory.MemoryId
  MemoryArn:
    Value: !GetAtt Memory.MemoryArn
  MemoryRef:
    Value: !Ref Memory
";

    let body = create_and_settle(&router, case, stack_name, template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body}"
    );
    let memory_id = published(case, &body, "MemoryId");
    assert_eq!(
        published(case, &body, "MemoryRef"),
        memory_id,
        "case {case:?}: the type's primary identifier is its memory id, so `Ref` must answer \
         it, got {body}"
    );

    let (status, reported) = get_memory(&router, &memory_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to resolve the id the stack published \
         ({memory_id}), got {status} with {reported}"
    );
    let memory = &reported["memory"];
    assert_eq!(
        memory["name"].as_str(),
        Some("stackMemoryOne"),
        "case {case:?}: expected the memory to carry the declared name, got {reported}"
    );
    assert_eq!(
        memory["description"].as_str(),
        Some("deployed by a CloudFormation stack"),
        "case {case:?}: expected the declared description, got {reported}"
    );
    assert_eq!(
        memory["eventExpiryDuration"].as_i64(),
        Some(45),
        "case {case:?}: expected the declared event expiry, got {reported}"
    );
    assert_eq!(
        memory["arn"].as_str(),
        Some(published(case, &body, "MemoryArn").as_str()),
        "case {case:?}: expected the published ARN to be the one the control plane minted, \
         got {reported}"
    );
    assert_eq!(
        memory["status"].as_str(),
        Some("ACTIVE"),
        "case {case:?}: expected a completed stack to have waited for the memory to be \
         active, got {reported}"
    );
}

#[tokio::test]
async fn a_gateway_template_is_accepted_and_both_its_resources_are_provisioned() {
    let router = build_router();
    let case = "a gateway and one of its targets in one template";
    let stack_name = "agentcore-gateway-stack";
    // A gateway's `name` is `^([0-9a-zA-Z][-]?){1,48}$` and its `authorizerType` is
    // `@required`; a target's tools are DECLARED here (an inline OpenAPI schema), so the
    // target settles ready without the gateway fetching anything from an endpoint.
    let template = "\
Resources:
  Gateway:
    Type: AWS::BedrockAgentCore::Gateway
    Properties:
      Name: Stack-Gateway-One
      Description: deployed by a CloudFormation stack
      RoleArn: arn:aws:iam::123456789012:role/GatewayRole
      AuthorizerType: AWS_IAM
  Target:
    Type: AWS::BedrockAgentCore::GatewayTarget
    Properties:
      GatewayIdentifier: !Ref Gateway
      Name: stackTargetOne
      Description: the target the gateway serves
      TargetConfiguration:
        Mcp:
          OpenApiSchema:
            InlinePayload: '{\"openapi\":\"3.0.0\"}'
      CredentialProviderConfigurations:
        - CredentialProviderType: GATEWAY_IAM_ROLE
Outputs:
  GatewayId:
    Value: !GetAtt Gateway.GatewayId
  GatewayArn:
    Value: !GetAtt Gateway.GatewayArn
  TargetId:
    Value: !GetAtt Target.TargetId
  TargetGatewayArn:
    Value: !GetAtt Target.GatewayArn
  TargetRef:
    Value: !Ref Target
";

    let body = create_and_settle(&router, case, stack_name, template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the template to be accepted and both resources \
         provisioned, got {body}"
    );

    let gateway_id = published(case, &body, "GatewayId");
    let target_id = published(case, &body, "TargetId");
    assert_eq!(
        published(case, &body, "TargetRef"),
        format!("{gateway_id}|{target_id}"),
        "case {case:?}: a target's primary identifier is the gateway and its own id, so \
         `Ref` must answer the pair, got {body}"
    );

    let (status, reported) = get_gateway(&router, &gateway_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to resolve the gateway the stack \
         published ({gateway_id}), got {status} with {reported}"
    );
    assert_eq!(
        reported["name"].as_str(),
        Some("Stack-Gateway-One"),
        "case {case:?}: expected the gateway to carry the declared name, got {reported}"
    );
    assert_eq!(
        reported["authorizerType"].as_str(),
        Some("AWS_IAM"),
        "case {case:?}: expected the declared authorizer type, got {reported}"
    );
    assert_eq!(
        reported["gatewayArn"].as_str(),
        Some(published(case, &body, "GatewayArn").as_str()),
        "case {case:?}: expected the published ARN to be the one the control plane minted, \
         got {reported}"
    );
    assert_eq!(
        reported["status"].as_str(),
        Some("READY"),
        "case {case:?}: expected a completed stack to have waited for the gateway to be \
         ready, got {reported}"
    );

    let (status, reported) = get_gateway_target(&router, &gateway_id, &target_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to resolve the target the stack \
         published ({target_id}), got {status} with {reported}"
    );
    assert_eq!(
        reported["name"].as_str(),
        Some("stackTargetOne"),
        "case {case:?}: expected the target to carry the declared name, got {reported}"
    );
    assert_eq!(
        reported["targetConfiguration"]["mcp"]["openApiSchema"]["inlinePayload"].as_str(),
        Some(r#"{"openapi":"3.0.0"}"#),
        "case {case:?}: expected the declared target configuration to reach the target, got \
         {reported}"
    );
    assert_eq!(
        reported["credentialProviderConfigurations"][0]["credentialProviderType"].as_str(),
        Some("GATEWAY_IAM_ROLE"),
        "case {case:?}: expected the declared credential provider, got {reported}"
    );
    assert_eq!(
        reported["gatewayArn"].as_str(),
        Some(published(case, &body, "TargetGatewayArn").as_str()),
        "case {case:?}: expected the target to publish the ARN of the gateway it is served \
         by, got {reported}"
    );
    assert_eq!(
        reported["status"].as_str(),
        Some("READY"),
        "case {case:?}: expected a completed stack to have waited for the target to be \
         ready, got {reported}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_agentcore_resources_it_created() {
    let router = build_router();
    let case = "a memory and a gateway deleted with their stack";
    let stack_name = "agentcore-deleted-stack";
    let template = "\
Resources:
  Memory:
    Type: AWS::BedrockAgentCore::Memory
    Properties:
      Name: deletedMemory
      EventExpiryDuration: 7
  Gateway:
    Type: AWS::BedrockAgentCore::Gateway
    Properties:
      Name: deleted-gateway
      RoleArn: arn:aws:iam::123456789012:role/GatewayRole
      AuthorizerType: NONE
Outputs:
  MemoryId:
    Value: !Ref Memory
  GatewayId:
    Value: !Ref Gateway
";

    let body = create_and_settle(&router, case, stack_name, template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body}"
    );
    let memory_id = published(case, &body, "MemoryId");
    let gateway_id = published(case, &body, "GatewayId");

    let (status, reported) = get_memory(&router, &memory_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to hold {memory_id}, got {status} with \
         {reported}"
    );
    let (status, reported) = get_gateway(&router, &gateway_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the control plane to hold {gateway_id}, got {status} with \
         {reported}"
    );

    let (status, body) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );

    // Each resource is gone from the control plane — the condition the deletion is waited
    // for, rather than a span.
    let seen = poll_until(case, "the memory to be gone from the control plane", || async {
        let (status, answer) = get_memory(&router, &memory_id).await;
        (status == 404, format!("HTTP {status} with {answer}"))
    })
    .await;
    assert!(
        seen.contains("404"),
        "case {case:?}: expected the memory to be gone, last saw {seen}"
    );
    let (_, answer) = get_memory(&router, &memory_id).await;
    assert_eq!(
        error_code(&answer),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException for the deleted memory, got \
         {answer}"
    );

    poll_until(
        case,
        "the gateway to be gone from the control plane",
        || async {
            let (status, answer) = get_gateway(&router, &gateway_id).await;
            (status == 404, format!("HTTP {status} with {answer}"))
        },
    )
    .await;
    let (_, answer) = get_gateway(&router, &gateway_id).await;
    assert_eq!(
        error_code(&answer),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException for the deleted gateway, got \
         {answer}"
    );
}

#[tokio::test]
async fn a_declaration_the_control_plane_refuses_fails_the_stack() {
    let router = build_router();
    let case = "an EventExpiryDuration outside the modelled range";
    let stack_name = "agentcore-refused-stack";
    // `CreateMemory$eventExpiryDuration` is `@range(min: 3, max: 365)`, so the control plane
    // refuses this declaration — which is what the stack must report rather than completing
    // with an id nothing created.
    let template = json!({
        "Resources": {
            "Memory": {
                "Type": "AWS::BedrockAgentCore::Memory",
                "Properties": {"Name": "refusedMemory", "EventExpiryDuration": 400}
            }
        },
        "Outputs": {"MemoryId": {"Value": {"Ref": "Memory"}}}
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the failed create to be rolled back rather than publish an \
         id the control plane never created, got {body}"
    );
    assert!(
        outputs_of(&body).is_empty(),
        "case {case:?}: expected no output for a resource nothing created, got {body}"
    );

    // And the control plane holds no memory at all: nothing was created.
    let (status, listed) = agentcore(&router, "POST", "/memories/", Some(json!({}))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListMemories to answer, got {status} with {listed}"
    );
    let count = listed["memories"].as_array().map(Vec::len).unwrap_or_default();
    assert_eq!(
        count, 0,
        "case {case:?}: expected the control plane to hold no memory, got {listed}"
    );
}

/// `GetAgentRuntime` on one id (`GET /runtimes/{agentRuntimeId}/`).
async fn get_runtime(router: &MultiServiceRouter, agent_runtime_id: &str) -> (u16, Value) {
    agentcore(router, "GET", &format!("/runtimes/{agent_runtime_id}/"), None).await
}

/// `GetMemory` on one id (`GET /memories/{memoryId}/details`).
async fn get_memory(router: &MultiServiceRouter, memory_id: &str) -> (u16, Value) {
    agentcore(
        router,
        "GET",
        &format!("/memories/{memory_id}/details"),
        None,
    )
    .await
}

/// `GetGateway` on one id (`GET /gateways/{gatewayIdentifier}/`).
async fn get_gateway(router: &MultiServiceRouter, gateway_id: &str) -> (u16, Value) {
    agentcore(router, "GET", &format!("/gateways/{gateway_id}/"), None).await
}

/// `GetGatewayTarget` on one gateway-and-target pair
/// (`GET /gateways/{gatewayIdentifier}/targets/{targetId}/`).
async fn get_gateway_target(
    router: &MultiServiceRouter,
    gateway_id: &str,
    target_id: &str,
) -> (u16, Value) {
    agentcore(
        router,
        "GET",
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
        None,
    )
    .await
}
