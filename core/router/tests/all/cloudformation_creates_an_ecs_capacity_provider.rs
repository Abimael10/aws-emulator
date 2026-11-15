// SPDX-License-Identifier: CC0-1.0

//! The capacity provider a CloudFormation stack declares is one Amazon ECS really HOLDS,
//! bound onto a group Amazon EC2 Auto Scaling really holds.
//!
//! The rule under test: **a resource type this emulator provisions is created in the service
//! that OWNS it, through that service's own API, under the identifier the stack publishes for
//! it** — for an `AWS::ECS::CapacityProvider` that identifier is the provider's name ("Ref
//! returns the capacity provider name"), which is the only handle Amazon ECS takes for one: a
//! `DescribeCapacityProviders` read, a `PutClusterCapacityProviders` association and a
//! `capacityProviderStrategy` entry each name a provider by nothing else.
//!
//! The binding follows the same rule one level down: the Auto Scaling group is a resource of
//! Amazon EC2 Auto Scaling's, so the group the provider is reported bound onto is the ARN
//! THAT service minted, whichever of the two forms
//! `AutoScalingGroupProvider$autoScalingGroupArn` admits the declaration named it by ("The
//! Amazon Resource Name (ARN) that identifies the Auto Scaling group, or the Auto Scaling
//! group name").
//!
//! Three crates have to agree for that to hold — CloudFormation makes the create, Amazon ECS
//! serves the provider afterwards, and Amazon EC2 Auto Scaling owns the group it binds onto,
//! over a launch template and a subnet Amazon EC2 owns — so this leg lives here, where the
//! whole mounted set is reachable. The per-case table over the declaration's own spellings
//! (the name the template states or CloudFormation generates, managed scaling enabled,
//! disabled or absent, the tags, both template languages) is the CloudFormation crate's own
//! suite, `cloudformation_creates_an_ecs_capacity_provider.rs`; what this adds is the same
//! rule read on the ASSEMBLED router.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region every resource below
/// belongs to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The `X-Amz-Target` prefix Amazon ECS's model fixes (service shape
/// `AmazonEC2ContainerServiceV20141113`), whose `sigv4Name` is `ecs`. It speaks awsJson1.1.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// How a read waits for the clock-owed spans of a deployment to come due: look again, under a
/// deadline generous enough that a loaded host still reaches it. What the case asserts is the
/// CONDITION, never how long it took.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(250);
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

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
    // Amazon EC2 Auto Scaling owns the group a capacity provider binds onto, and Amazon EC2
    // the launch template it launches from and the subnet it launches into: the stack really
    // creates all three, so each owning service is mounted beside CloudFormation or those
    // resources' creates are refused and the stack fails.
    router.register_service(
        service_com_amazonaws_autoscaling_autoscaling_2011_01_01::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
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

/// Call one operation over an awsQuery wire: the form-encoded body an AWS SDK sends, answered
/// with the HTTP status and the XML body.
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in params {
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

/// One CloudFormation call.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "cloudformation", "2010-05-15", action, params).await
}

/// One Amazon EC2 Auto Scaling call.
async fn autoscaling(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "autoscaling", "2011-01-01", action, params).await
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

/// `DescribeCapacityProviders` for ONE provider, with its tags asked for.
async fn describe_provider(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    ecs(
        router,
        "DescribeCapacityProviders",
        &json!({"capacityProviders": [name], "include": ["TAGS"]}),
    )
    .await
}

/// The ARN Amazon EC2 Auto Scaling reports for the group it holds under `name` — the identity
/// THAT service minted, which is what a provider's binding has to name.
async fn group_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = autoscaling(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Auto Scaling to report the group {name:?}, got {status} {body}"
    );
    elements(&body, "AutoScalingGroupARN")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no AutoScalingGroupARN for {name:?} in {body}"))
}

/// Read a stack until it reports a status that is no longer in progress, and answer that
/// read's body. The deadline is only here so a failure is a failure rather than a hang.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        if !opt_element(&body, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack} to settle, last read {body}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
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

/// The template this leg deploys: the fleet the provider binds onto, and the provider itself.
const TEMPLATE: &str = concat!(
    "Resources:\n",
    "  Vpc:\n",
    "    Type: AWS::EC2::VPC\n",
    "    Properties:\n",
    "      CidrBlock: 10.79.0.0/16\n",
    "  SubnetA:\n",
    "    Type: AWS::EC2::Subnet\n",
    "    Properties:\n",
    "      VpcId: !Ref Vpc\n",
    "      CidrBlock: 10.79.1.0/24\n",
    "      AvailabilityZone: us-east-1a\n",
    "  Lt:\n",
    "    Type: AWS::EC2::LaunchTemplate\n",
    "    Properties:\n",
    "      LaunchTemplateData:\n",
    "        ImageId: ami-00000000000000079\n",
    "        InstanceType: t3.micro\n",
    "  Asg:\n",
    "    Type: AWS::AutoScaling::AutoScalingGroup\n",
    "    Properties:\n",
    "      MinSize: \"0\"\n",
    "      MaxSize: \"4\"\n",
    "      DesiredCapacity: \"0\"\n",
    "      NewInstancesProtectedFromScaleIn: true\n",
    "      VPCZoneIdentifier:\n",
    "        - !Ref SubnetA\n",
    "      LaunchTemplate:\n",
    "        LaunchTemplateId: !Ref Lt\n",
    "        Version: !GetAtt Lt.LatestVersionNumber\n",
    "  Cap:\n",
    "    Type: AWS::ECS::CapacityProvider\n",
    "    Properties:\n",
    "      AutoScalingGroupProvider:\n",
    "        AutoScalingGroupArn: !Ref Asg\n",
    "        ManagedScaling:\n",
    "          Status: ENABLED\n",
    "          TargetCapacity: 80\n",
    "        ManagedTerminationProtection: ENABLED\n",
    "      Tags:\n",
    "        - Key: Fleet\n",
    "          Value: router\n",
    "Outputs:\n",
    "  ProviderName:\n",
    "    Value: !Ref Cap\n",
    "  GroupName:\n",
    "    Value: !Ref Asg\n",
);

#[tokio::test]
async fn a_declared_capacity_provider_is_one_amazon_ecs_holds() {
    let router = build_router();
    let case = "a managed-scaling provider over the Auto Scaling group of the same stack";
    let stack = "router-cfn-capacity-provider";

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", TEMPLATE)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = poll_to_terminal(&router, case, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete; failed resources {:?}",
        failures_of(&router, stack).await,
    );

    let outputs = outputs_of(&described);
    let name = outputs
        .get("ProviderName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ProviderName output in {described}"));
    let group = outputs
        .get("GroupName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no GroupName output in {described}"));

    // THE RULE: the provider the stack claims to have created is a provider Amazon ECS holds,
    // under exactly the name the stack published.
    let (status, answer) = describe_provider(&router, &name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon ECS to answer for {name:?}, got {status} with {answer}"
    );
    let providers = answer["capacityProviders"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        (providers.len(), &answer["failures"]),
        (1, &json!([])),
        "case {case:?}: expected exactly one provider named {name:?} and no failures, got \
         {answer}"
    );
    let provider = &providers[0];
    let expected_arn = format!("arn:aws:ecs:{REGION}:{ACCOUNT_ID}:capacity-provider/{name}");
    assert_eq!(
        (
            provider["name"].as_str(),
            provider["status"].as_str(),
            provider["type"].as_str(),
            provider["capacityProviderArn"].as_str(),
        ),
        (
            Some(name.as_str()),
            Some("ACTIVE"),
            Some("EC2_AUTOSCALING"),
            Some(expected_arn.as_str()),
        ),
        "case {case:?}: expected the provider reported active over the account's own Auto \
         Scaling capacity under {expected_arn:?}, got {provider}"
    );

    // The group it is bound onto is the one Amazon EC2 Auto Scaling holds, reported as the ARN
    // THAT service minted — the declaration named the group by its NAME.
    let expected_group = group_arn(&router, case, &group).await;
    let binding = &provider["autoScalingGroupProvider"];
    assert_eq!(
        (
            binding["autoScalingGroupArn"].as_str(),
            binding["managedScaling"]["status"].as_str(),
            binding["managedScaling"]["targetCapacity"].as_i64(),
            binding["managedTerminationProtection"].as_str(),
        ),
        (
            Some(expected_group.as_str()),
            Some("ENABLED"),
            Some(80),
            Some("ENABLED"),
        ),
        "case {case:?}: expected the declared binding onto the group {group:?} \
         ({expected_group:?}), got {binding}"
    );

    // ...and the tag the declaration asked for is the provider's own.
    assert_eq!(
        provider["tags"],
        json!([{"key": "Fleet", "value": "router"}]),
        "case {case:?}: expected the provider to carry the declared tag, got {provider}"
    );
}
