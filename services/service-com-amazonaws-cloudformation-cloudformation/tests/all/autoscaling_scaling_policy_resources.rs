// SPDX-License-Identifier: CC0-1.0

//! The `AWS::AutoScaling::ScalingPolicy` a stack declares for one of its Auto Scaling groups,
//! read back through the wire of the service that OWNS it.
//!
//! RULE: a scaling policy a template declares is PUT in Amazon EC2 Auto Scaling itself, with
//! the declaration's own members as `PutScalingPolicy`'s request — whatever policy type it is —
//! so `DescribePolicies(AutoScalingGroupName=<group>)` answers with exactly that one policy,
//! carrying the configuration the template declared and, for a target-tracking policy, the
//! CloudWatch alarms Amazon CloudWatch really holds for it.
//!
//! The rows differ along what the rule generalises over: the POLICY TYPE and therefore the shape
//! of the declaration — a target-tracking configuration over a predefined metric, one over a
//! customized metric with scale-in disabled, a step policy whose adjustments are a LIST, and a
//! simple policy whose adjustment and cooldown are scalars the type spells as a string.

use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::cloudwatch;
use crate::harness::stack_event_reasons;
use crate::stack_lifecycle::{
    autoscaling, create_stack, list_members, router, settled_stack, xml_blocks, xml_text, REGION,
};

/// The group, its launch template and the output every row shares, so each row's template
/// differs only in the POLICY it declares.
///
/// The group launches nothing (`MinSize`/`MaxSize` are zero), because what the rows are about is
/// the policy attached to it; its launch template names an image id in the one form Amazon EC2
/// reads one as (`ami-` plus 17 hexadecimal characters).
macro_rules! policy_template {
    ($policy:expr) => {
        concat!(
            "Resources:\n",
            "  Lt:\n",
            "    Type: AWS::EC2::LaunchTemplate\n",
            "    Properties:\n",
            "      LaunchTemplateData:\n",
            "        ImageId: ami-00000000000000001\n",
            "        InstanceType: t3.micro\n",
            "  Asg:\n",
            "    Type: AWS::AutoScaling::AutoScalingGroup\n",
            "    Properties:\n",
            "      MinSize: \"0\"\n",
            "      MaxSize: \"0\"\n",
            "      AvailabilityZones:\n",
            "        - us-east-1a\n",
            "      LaunchTemplate:\n",
            "        LaunchTemplateId: !Ref Lt\n",
            "        Version: !GetAtt Lt.LatestVersionNumber\n",
            $policy,
            "Outputs:\n",
            "  GroupName:\n",
            "    Value: !Ref Asg\n",
            "  PolicyName:\n",
            "    Value: !GetAtt Policy.PolicyName\n",
        )
    };
}

/// One policy a row declares, and what Amazon EC2 Auto Scaling must report for it.
struct PolicyCase {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The `PolicyType` the read must report — the declared one, or `SimpleScaling` for a
    /// declaration that names none ("SimpleScaling (default)",
    /// `PutScalingPolicyType$PolicyType`).
    policy_type: &'static str,
    /// Members the read must report for the policy, as (element, value) pairs: this is what
    /// says the declaration reached that service as the document the template wrote.
    reported: &'static [(&'static str, &'static str)],
    /// How many CloudWatch alarms the policy must report — the alarms that DRIVE a
    /// target-tracking policy, and none for a policy of any other type.
    alarms: usize,
}

const POLICY_CASES: &[PolicyCase] = &[
    PolicyCase {
        case: "a target-tracking policy over a predefined metric",
        stack: "asg-policy-target-tracking",
        template: policy_template!(concat!(
            "  Policy:\n",
            "    Type: AWS::AutoScaling::ScalingPolicy\n",
            "    Properties:\n",
            "      AutoScalingGroupName: !Ref Asg\n",
            "      PolicyType: TargetTrackingScaling\n",
            "      TargetTrackingConfiguration:\n",
            "        PredefinedMetricSpecification:\n",
            "          PredefinedMetricType: ASGAverageCPUUtilization\n",
            "        TargetValue: 50\n",
        )),
        policy_type: "TargetTrackingScaling",
        reported: &[
            ("PredefinedMetricType", "ASGAverageCPUUtilization"),
            ("TargetValue", "50.0"),
            // "The default is false" (`TargetTrackingConfiguration$DisableScaleIn`): the
            // policy in force carries the switch the put resolved, not the request's silence.
            ("DisableScaleIn", "false"),
        ],
        // The scale-out alarm and the scale-in one.
        alarms: 2,
    },
    PolicyCase {
        case: "a target-tracking policy over a customized metric, with scale-in disabled",
        stack: "asg-policy-customized-metric",
        template: policy_template!(concat!(
            "  Policy:\n",
            "    Type: AWS::AutoScaling::ScalingPolicy\n",
            "    Properties:\n",
            "      AutoScalingGroupName: !Ref Asg\n",
            "      PolicyType: TargetTrackingScaling\n",
            "      EstimatedInstanceWarmup: 90\n",
            "      TargetTrackingConfiguration:\n",
            "        DisableScaleIn: true\n",
            "        TargetValue: 1200\n",
            "        CustomizedMetricSpecification:\n",
            "          MetricName: OrdersPerInstance\n",
            "          Namespace: Shop/Orders\n",
            "          Statistic: Average\n",
            "          Dimensions:\n",
            "            - Name: Queue\n",
            "              Value: orders\n",
        )),
        policy_type: "TargetTrackingScaling",
        reported: &[
            ("MetricName", "OrdersPerInstance"),
            ("Namespace", "Shop/Orders"),
            ("Statistic", "Average"),
            ("TargetValue", "1200.0"),
            ("DisableScaleIn", "true"),
            ("EstimatedInstanceWarmup", "90"),
        ],
        // Scale-in is disabled, so the policy holds the scale-out alarm alone.
        alarms: 1,
    },
    PolicyCase {
        case: "a step-scaling policy whose adjustments are a list",
        stack: "asg-policy-step",
        template: policy_template!(concat!(
            "  Policy:\n",
            "    Type: AWS::AutoScaling::ScalingPolicy\n",
            "    Properties:\n",
            "      AutoScalingGroupName: !Ref Asg\n",
            "      PolicyType: StepScaling\n",
            "      AdjustmentType: ChangeInCapacity\n",
            "      MetricAggregationType: Maximum\n",
            "      StepAdjustments:\n",
            "        - MetricIntervalLowerBound: 0\n",
            "          MetricIntervalUpperBound: 10\n",
            "          ScalingAdjustment: 1\n",
            "        - MetricIntervalLowerBound: 10\n",
            "          ScalingAdjustment: 3\n",
        )),
        policy_type: "StepScaling",
        reported: &[
            ("AdjustmentType", "ChangeInCapacity"),
            ("MetricAggregationType", "Maximum"),
        ],
        alarms: 0,
    },
    PolicyCase {
        case: "a simple-scaling policy, the type a declaration naming none asks for",
        stack: "asg-policy-simple",
        template: policy_template!(concat!(
            "  Policy:\n",
            "    Type: AWS::AutoScaling::ScalingPolicy\n",
            "    Properties:\n",
            "      AutoScalingGroupName: !Ref Asg\n",
            "      AdjustmentType: PercentChangeInCapacity\n",
            "      MinAdjustmentMagnitude: 2\n",
            "      ScalingAdjustment: -1\n",
            "      Cooldown: \"300\"\n",
        )),
        policy_type: "SimpleScaling",
        reported: &[
            ("AdjustmentType", "PercentChangeInCapacity"),
            ("MinAdjustmentMagnitude", "2"),
            ("ScalingAdjustment", "-1"),
            ("Cooldown", "300"),
        ],
        alarms: 0,
    },
];

#[tokio::test]
async fn a_scaling_policy_a_stack_declares_is_the_policy_that_service_reports() {
    for row in POLICY_CASES {
        let case = row.case;
        let router = router();
        let published = deployed(&router, case, row.stack, row.template).await;
        let group = output(case, &published, "GroupName");
        let name = output(case, &published, "PolicyName");

        // Exactly one policy on the group: the one the template declared, under the name the
        // stack published for it.
        let policies = group_policies(case, &router, &group).await;
        assert_eq!(
            policies.len(),
            1,
            "case {case:?}: expected the one declared policy, got {policies:?}"
        );
        let policy = &policies[0];
        assert_eq!(
            (
                xml_text(policy, "PolicyName").as_deref(),
                xml_text(policy, "AutoScalingGroupName").as_deref(),
                xml_text(policy, "PolicyType").as_deref(),
            ),
            (Some(name.as_str()), Some(group.as_str()), Some(row.policy_type)),
            "case {case:?}: expected the declared policy on the declared group, got {policy}"
        );
        for (element, value) in row.reported {
            assert_eq!(
                xml_text(policy, element).as_deref(),
                Some(*value),
                "case {case:?}: expected {element} to be reported as {value:?}, got {policy}"
            );
        }

        // A step policy's adjustments are reported as the list the declaration stated.
        if row.policy_type == "StepScaling" {
            let steps = list_members(policy, "StepAdjustments");
            assert_eq!(
                steps
                    .iter()
                    .map(|step| xml_text(step, "ScalingAdjustment").unwrap_or_default())
                    .collect::<Vec<String>>(),
                vec!["1".to_string(), "3".to_string()],
                "case {case:?}: expected both declared steps, got {steps:?}"
            );
        }

        // The alarms the policy reports are alarms Amazon CloudWatch really holds: a
        // target-tracking policy IS those alarms, so each one is read back through that
        // service's own wire.
        let alarms = list_members(policy, "Alarms");
        assert_eq!(
            alarms.len(),
            row.alarms,
            "case {case:?}: expected {} alarm(s) for the policy, got {alarms:?}",
            row.alarms
        );
        for alarm in &alarms {
            let alarm_name = xml_text(alarm, "AlarmName").unwrap_or_default();
            let (status, document) = cloudwatch(
                &router,
                "DescribeAlarms",
                &serde_json::json!({ "AlarmNames": [alarm_name] }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeAlarms({alarm_name:?}) must answer 200, got {status} \
                 {document}"
            );
            let reported = document
                .get("MetricAlarms")
                .and_then(serde_json::Value::as_array)
                .map(|held| {
                    held.iter()
                        .filter_map(|alarm| alarm.get("AlarmName").and_then(|n| n.as_str()))
                        .map(ToString::to_string)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            assert_eq!(
                reported,
                vec![alarm_name.clone()],
                "case {case:?}: expected CloudWatch to hold the policy's alarm \
                 {alarm_name:?}, got {document}"
            );
        }
    }
}

/// Deploy one row's template, wait for the stack to settle and answer its `DescribeStacks`
/// body.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} {body}"
    );
    let described = settled_stack(router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {described} with reasons {:?}",
        stack_event_reasons(router, REGION, stack).await
    );
    described
}

/// One output's `OutputValue`, asserted PRESENT: an output reported with a key and no value is
/// one a client cannot use at all.
fn output(case: &str, described: &str, key: &str) -> String {
    let outputs = xml_blocks(described, "Outputs")
        .into_iter()
        .next()
        .unwrap_or_default();
    xml_blocks(&outputs, "member")
        .into_iter()
        .find(|entry| xml_text(entry, "OutputKey").as_deref() == Some(key))
        .and_then(|entry| xml_text(&entry, "OutputValue"))
        .unwrap_or_else(|| panic!("case {case:?}: no output named {key:?} in {described}"))
}

/// Every policy Amazon EC2 Auto Scaling reports for `group`, each as the XML of its own
/// `ScalingPolicies` entry.
async fn group_policies(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<String> {
    let (status, body) = autoscaling(
        router,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribePolicies({group}) must answer 200, got {status} {body}"
    );
    list_members(&body, "ScalingPolicies")
}
