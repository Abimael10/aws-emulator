// SPDX-License-Identifier: CC0-1.0

//! An `AWS::ECS::CapacityProvider` a stack declares is CREATED in Amazon ECS itself.
//!
//! The rule under test: **a resource type this emulator provisions is created in the service
//! that OWNS it, through that service's own API, under the identifier the stack publishes for
//! it** — for this type that identifier is the provider's name ("Ref returns the capacity
//! provider name"), and the name is the only handle Amazon ECS takes for one:
//! `DescribeCapacityProviders` reads a provider by it, `PutClusterCapacityProviders`
//! associates one with a cluster by it, and a `capacityProviderStrategy` entry on `RunTask`
//! or `CreateService` names one by nothing else. A stack that reported the resource created
//! while nothing created the provider publishes a name that resolves to nothing, so the first
//! ECS call a client makes with what the stack handed it answers no provider at all.
//!
//! The binding the provider reports follows the same rule one level down: the Auto Scaling
//! group is a resource of Amazon EC2 Auto Scaling's, so the group a provider is reported
//! bound onto is the ARN THAT service minted — whichever of the two forms
//! `AutoScalingGroupProvider$autoScalingGroupArn` admits the declaration named it by ("The
//! Amazon Resource Name (ARN) that identifies the Auto Scaling group, or the Auto Scaling
//! group name").
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the
//! provider or leaves CloudFormation to generate the name, how the declaration spells the Auto
//! Scaling group (its name, through `!Ref` of the group resource, and its full ARN, through
//! `!GetAtt Asg.Arn`), whether managed scaling is enabled with every scaling member stated,
//! disabled, or not declared at all, how many tags the declaration carries, and both template
//! spellings (YAML and JSON).
//!
//! The second test is the same rule read the other way: a declaration naming a group Amazon
//! EC2 Auto Scaling holds none of creates NO provider and the stack says so, so what makes
//! the stack's handle work is the state the deployment wrote and not the shape of the string.
//! The third is the end of the resource's life: the provider a stack created is deleted in
//! Amazon ECS when the stack is, which is what makes it the stack's resource rather than a
//! leftover.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation and for
//! Amazon EC2 Auto Scaling, awsJson1.1 for Amazon ECS — through the one router `harness.rs`
//! builds, so what is asserted is what a client that deploys the template and then reads the
//! resources back observes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::ecs;
use crate::harness::{settled_stack_status, stack_event_reasons};
use crate::stack_lifecycle::{
    autoscaling, call, create_stack, describe_stack, member, resource_event_reason, router,
    xml_text, xml_texts, ACCOUNT_ID, REGION,
};

/// How a read waits for a clock-owed span to come due: look again, under a deadline generous
/// enough that a loaded host still reaches it. What every case asserts is the CONDITION, never
/// how long it took.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// The fleet every row's provider binds onto: a VPC, a subnet of it, the launch template the
/// group launches from and the group itself. Spelled once, so each row's template differs only
/// in the capacity provider it declares.
const FLEET_YAML: &str = concat!(
    "Resources:\n",
    "  Vpc:\n",
    "    Type: AWS::EC2::VPC\n",
    "    Properties:\n",
    "      CidrBlock: 10.77.0.0/16\n",
    "  SubnetA:\n",
    "    Type: AWS::EC2::Subnet\n",
    "    Properties:\n",
    "      VpcId: !Ref Vpc\n",
    "      CidrBlock: 10.77.1.0/24\n",
    "      AvailabilityZone: us-east-1a\n",
    "  Lt:\n",
    "    Type: AWS::EC2::LaunchTemplate\n",
    "    Properties:\n",
    "      LaunchTemplateData:\n",
    "        ImageId: ami-00000000000000077\n",
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
);

/// The outputs every YAML row publishes: the provider's own handle and the group's name, which
/// is what the assertions read the group's ARN back by.
const OUTPUTS_YAML: &str = concat!(
    "Outputs:\n",
    "  ProviderName:\n",
    "    Value: !Ref Cap\n",
    "  GroupName:\n",
    "    Value: !Ref Asg\n",
);

/// One deployment: the provider the template declares, and what Amazon ECS must then report
/// about the provider the stack claims to have created.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Cap` declaration appended to [`FLEET_YAML`], or — where `json` is set — the row's
    /// whole template document.
    declaration: &'static str,
    /// Whether `declaration` is the row's whole JSON template rather than a YAML fragment.
    json: bool,
    /// The name the template states, or `None` where CloudFormation generates it.
    declared_name: Option<&'static str>,
    /// The `managedScaling$status` the provider must report, or `None` where the declaration
    /// carries no `ManagedScaling` at all.
    managed_scaling_status: Option<&'static str>,
    /// The numeric `managedScaling` members the provider must report, as (member, value).
    managed_scaling: &'static [(&'static str, i64)],
    /// The two options beside managed scaling: what the provider must report, which is the
    /// value the declaration states, or the one Amazon ECS applies for a declaration that
    /// states none (`managedDraining` is turned on for such a provider, while
    /// `managedTerminationProtection` stays off).
    managed_termination_protection: Option<&'static str>,
    managed_draining: Option<&'static str>,
    /// The tags the provider must carry.
    tags: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "managed scaling ENABLED with every scaling member stated, the group named by \
               its NAME, the provider's own name declared, and a tag",
        stack: "cfn-cp-managed-scaling",
        declaration: concat!(
            "  Cap:\n",
            "    Type: AWS::ECS::CapacityProvider\n",
            "    Properties:\n",
            "      Name: cfn-cp-declared-name\n",
            "      AutoScalingGroupProvider:\n",
            "        AutoScalingGroupArn: !Ref Asg\n",
            "        ManagedScaling:\n",
            "          Status: ENABLED\n",
            "          TargetCapacity: 75\n",
            "          MinimumScalingStepSize: 2\n",
            "          MaximumScalingStepSize: 20\n",
            "          InstanceWarmupPeriod: 120\n",
            "        ManagedTerminationProtection: ENABLED\n",
            "      Tags:\n",
            "        - Key: Fleet\n",
            "          Value: batch\n",
        ),
        json: false,
        declared_name: Some("cfn-cp-declared-name"),
        managed_scaling_status: Some("ENABLED"),
        managed_scaling: &[
            ("targetCapacity", 75),
            ("minimumScalingStepSize", 2),
            ("maximumScalingStepSize", 20),
            ("instanceWarmupPeriod", 120),
        ],
        managed_termination_protection: Some("ENABLED"),
        managed_draining: Some("ENABLED"),
        tags: &[("Fleet", "batch")],
    },
    Row {
        case: "managed scaling DISABLED, the group named by its full ARN, and the provider's \
               name left to CloudFormation",
        stack: "cfn-cp-generated-name",
        declaration: concat!(
            "  Cap:\n",
            "    Type: AWS::ECS::CapacityProvider\n",
            "    Properties:\n",
            "      AutoScalingGroupProvider:\n",
            "        AutoScalingGroupArn: !GetAtt Asg.Arn\n",
            "        ManagedScaling:\n",
            "          Status: DISABLED\n",
            "        ManagedDraining: DISABLED\n",
        ),
        json: false,
        declared_name: None,
        managed_scaling_status: Some("DISABLED"),
        managed_scaling: &[],
        managed_termination_protection: Some("DISABLED"),
        managed_draining: Some("DISABLED"),
        tags: &[],
    },
    Row {
        case: "no ManagedScaling member at all, declared in JSON, with two tags",
        stack: "cfn-cp-unmanaged-scaling",
        declaration: r#"{
          "Resources": {
            "Vpc": {"Type": "AWS::EC2::VPC", "Properties": {"CidrBlock": "10.78.0.0/16"}},
            "SubnetA": {
              "Type": "AWS::EC2::Subnet",
              "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": "10.78.1.0/24",
                "AvailabilityZone": "us-east-1b"
              }
            },
            "Lt": {
              "Type": "AWS::EC2::LaunchTemplate",
              "Properties": {
                "LaunchTemplateData": {
                  "ImageId": "ami-00000000000000078",
                  "InstanceType": "t3.small"
                }
              }
            },
            "Asg": {
              "Type": "AWS::AutoScaling::AutoScalingGroup",
              "Properties": {
                "MinSize": "0",
                "MaxSize": "2",
                "VPCZoneIdentifier": [{"Ref": "SubnetA"}],
                "LaunchTemplate": {
                  "LaunchTemplateId": {"Ref": "Lt"},
                  "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]}
                }
              }
            },
            "Cap": {
              "Type": "AWS::ECS::CapacityProvider",
              "Properties": {
                "Name": "cfn-cp-unmanaged",
                "AutoScalingGroupProvider": {"AutoScalingGroupArn": {"Ref": "Asg"}},
                "Tags": [
                  {"Key": "Fleet", "Value": "unmanaged"},
                  {"Key": "Owner", "Value": "platform"}
                ]
              }
            }
          },
          "Outputs": {
            "ProviderName": {"Value": {"Ref": "Cap"}},
            "GroupName": {"Value": {"Ref": "Asg"}}
          }
        }"#,
        json: true,
        declared_name: Some("cfn-cp-unmanaged"),
        managed_scaling_status: None,
        managed_scaling: &[],
        managed_termination_protection: Some("DISABLED"),
        managed_draining: Some("ENABLED"),
        tags: &[("Fleet", "unmanaged"), ("Owner", "platform")],
    },
];

/// The template one row deploys.
fn template_of(row: &Row) -> String {
    if row.json {
        return row.declaration.to_string();
    }
    format!("{FLEET_YAML}{}{OUTPUTS_YAML}", row.declaration)
}

/// `DescribeCapacityProviders` for ONE provider, with its tags asked for: the status and the
/// answer, which carries the provider in `capacityProviders` or the reference in `failures`.
async fn describe_provider(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    ecs(
        router,
        "DescribeCapacityProviders",
        &json!({"capacityProviders": [name], "include": ["TAGS"]}),
    )
    .await
}

/// Every capacity-provider name the account's own unfiltered, fully paged listing reports.
async fn listed_providers(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({});
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let (status, answer) = ecs(router, "DescribeCapacityProviders", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the listing to answer 200, got {status} with {answer}"
        );
        names.extend(
            answer["capacityProviders"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|provider| provider["name"].as_str().map(str::to_string)),
        );
        token = answer["nextToken"].as_str().map(str::to_string);
        if token.is_none() {
            return names;
        }
    }
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
    xml_texts(&body, "AutoScalingGroupARN")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no AutoScalingGroupARN for {name:?} in {body}"))
}

/// Deploy a template and answer the status its create settled on, polled the way a client's
/// own waiter polls.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    settled_stack_status(case, router, REGION, stack).await
}

/// One output's `OutputValue`, which is where a client reads the identifier a declared
/// resource was created under.
async fn output(router: &MultiServiceRouter, case: &str, stack: &str, key: &str) -> String {
    let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
    );
    crate::harness::xml_blocks(&body, "member")
        .iter()
        .find(|entry| xml_text(entry, "OutputKey").as_deref() == Some(key))
        .and_then(|entry| xml_text(entry, "OutputValue"))
        .unwrap_or_else(|| panic!("case {case:?}: no {key:?} output with a value in {body}"))
}

/// RULE: the capacity provider a stack declares is one Amazon ECS holds, under the name the
/// stack publishes, bound onto the Auto Scaling group the declaration named.
#[tokio::test]
async fn a_declared_capacity_provider_is_one_amazon_ecs_holds() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let settled = deploy(&router, case, row.stack, &template_of(row)).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {settled:?} because {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        // The handle the stack publishes for the resource: "Ref returns the capacity provider
        // name", which is the declared name where the template states one and the name
        // CloudFormation generated where it does not.
        let name = output(&router, case, row.stack, "ProviderName").await;
        match row.declared_name {
            Some(stated) => assert_eq!(
                name, stated,
                "case {case:?}: expected the declared name to be the published handle, got \
                 {name:?}"
            ),
            None => assert!(
                name.starts_with(&format!("{}-Cap-", row.stack)),
                "case {case:?}: expected a generated <stack>-<logical id>-<unique id> name, \
                 got {name:?}"
            ),
        }

        // THE RULE: the provider the stack claims to have created is a provider Amazon ECS
        // holds, under exactly that name.
        let (status, answer) = describe_provider(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon ECS to answer for {name:?}, got {status} {answer}"
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

        // The group it is bound onto is the one Amazon EC2 Auto Scaling holds, reported as the
        // ARN THAT service minted — whichever form the declaration named it by.
        let group = output(&router, case, row.stack, "GroupName").await;
        let group_arn = group_arn(&router, case, &group).await;
        let binding = &provider["autoScalingGroupProvider"];
        assert_eq!(
            binding["autoScalingGroupArn"].as_str(),
            Some(group_arn.as_str()),
            "case {case:?}: expected the binding to name the group {group:?} by the ARN Auto \
             Scaling minted ({group_arn:?}), got {binding}"
        );
        assert_eq!(
            binding["managedScaling"]["status"].as_str(),
            row.managed_scaling_status,
            "case {case:?}: expected the declared managed-scaling status, got {binding}"
        );
        for (member, declared) in row.managed_scaling {
            assert_eq!(
                binding["managedScaling"][member].as_i64(),
                Some(*declared),
                "case {case:?}: expected managedScaling.{member} to be {declared}, got {binding}"
            );
        }
        assert_eq!(
            (
                binding["managedTerminationProtection"].as_str(),
                binding["managedDraining"].as_str(),
            ),
            (row.managed_termination_protection, row.managed_draining),
            "case {case:?}: expected the termination-protection and draining options the \
             declaration states, or the ones Amazon ECS applies for the ones it does not, \
             got {binding}"
        );

        // The tags the declaration asked for are the provider's own, read back through the
        // `include` that reports them.
        let reported_tags: Vec<(String, String)> = provider["tags"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|tag| {
                Some((
                    tag["key"].as_str()?.to_string(),
                    tag["value"].as_str()?.to_string(),
                ))
            })
            .collect();
        for (key, value) in row.tags {
            assert!(
                reported_tags.contains(&((*key).to_string(), (*value).to_string())),
                "case {case:?}: expected the provider to carry the tag {key:?}={value:?}, got \
                 {reported_tags:?}"
            );
        }

        // ...and the account's own unfiltered listing reports it, which is where a client that
        // did not keep the name finds the providers it has.
        let listed = listed_providers(&router, case).await;
        assert!(
            listed.contains(&name),
            "case {case:?}: expected the account's listing to carry {name:?}, got {listed:?}"
        );
    }
}

/// RULE, read the other way: a provider Amazon ECS refuses is created nowhere, so the stack
/// reports the refusal instead of publishing a handle that resolves to nothing.
#[tokio::test]
async fn a_provider_over_a_group_nothing_created_is_no_resource_at_all() {
    let router = router();
    let case = "a declaration naming an Auto Scaling group the account does not hold";
    let stack = "cfn-cp-unresolved-group";
    let template = concat!(
        "Resources:\n",
        "  Cap:\n",
        "    Type: AWS::ECS::CapacityProvider\n",
        "    Properties:\n",
        "      Name: cfn-cp-unresolved\n",
        "      AutoScalingGroupProvider:\n",
        "        AutoScalingGroupArn: a-group-nothing-created\n",
    );

    let settled = deploy(&router, case, stack, template).await;
    assert!(
        settled.starts_with("ROLLBACK_") || settled == "CREATE_FAILED",
        "case {case:?}: expected the stack not to report the resource created, got {settled:?} \
         because {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let reason = resource_event_reason(&router, stack, "Cap", "CREATE_FAILED").await;
    assert!(
        reason.contains("Auto Scaling group"),
        "case {case:?}: expected the resource to carry Amazon ECS's own refusal of the group, \
         got {reason:?}"
    );

    // Nothing was created anywhere: the name the declaration asked for resolves to no
    // provider, which is what makes the handle a deployed stack publishes meaningful.
    let (status, answer) = describe_provider(&router, "cfn-cp-unresolved").await;
    assert_eq!(
        (status, answer["capacityProviders"].as_array().map(Vec::len)),
        (200, Some(0)),
        "case {case:?}: expected Amazon ECS to hold no provider, got {status} with {answer}"
    );
}

/// RULE at the end of the resource's life: the provider the stack created is deleted in
/// Amazon ECS when the stack is deleted, which that service reports as the `INACTIVE` status
/// a successfully deleted provider has.
#[tokio::test]
async fn deleting_the_stack_deletes_the_provider_it_created() {
    let router = router();
    let case = "a provider deleted with its stack";
    let stack = "cfn-cp-deleted";
    let template = format!(
        "{FLEET_YAML}{}{OUTPUTS_YAML}",
        concat!(
            "  Cap:\n",
            "    Type: AWS::ECS::CapacityProvider\n",
            "    Properties:\n",
            "      Name: cfn-cp-deleted-provider\n",
            "      AutoScalingGroupProvider:\n",
            "        AutoScalingGroupArn: !Ref Asg\n",
            "        ManagedScaling:\n",
            "          Status: ENABLED\n",
            "          TargetCapacity: 90\n",
        )
    );

    let settled = deploy(&router, case, stack, &template).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {settled:?} because {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let name = output(&router, case, stack, "ProviderName").await;
    let (_, before) = describe_provider(&router, &name).await;
    assert_eq!(
        before["capacityProviders"][0]["status"].as_str(),
        Some("ACTIVE"),
        "case {case:?}: expected the provider to be active before the delete, got {before}"
    );

    let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {body}"
    );

    // The provider the stack created is DELETED in Amazon ECS: `INACTIVE` from the delete on
    // — "when a capacity provider is successfully deleted, it has an INACTIVE status" — with
    // the release of the capacity it bound onto reported on its own update status until that
    // service finishes it. Waited for as the CONDITION, under a deadline generous enough that
    // a loaded host cannot decide the outcome.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, answer) = describe_provider(&router, &name).await;
        let provider = &answer["capacityProviders"][0];
        if provider["updateStatus"].as_str() == Some("DELETE_COMPLETE") {
            assert_eq!(
                provider["status"].as_str(),
                Some("INACTIVE"),
                "case {case:?}: expected a deleted provider to be inactive, got {answer}"
            );
            break;
        }
        let (_, stack_body) = describe_stack(&router, stack).await;
        assert!(
            !stack_body.contains("DELETE_FAILED"),
            "case {case:?}: expected the delete to succeed, got {stack_body}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected Amazon ECS to report {name:?} deleted, last read {answer}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}
