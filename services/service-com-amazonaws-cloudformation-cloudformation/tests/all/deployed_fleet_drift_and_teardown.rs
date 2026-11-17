// SPDX-License-Identifier: CC0-1.0

//! What becomes of the Auto Scaling GROUP a stack deployed once its instances are running: the
//! drift a client probes for, and the teardown it asks for.
//!
//! Two rules, read through the wire of the service that owns each resource:
//!
//! 1. The ACTUAL configuration a drift read reports is projected onto the FORM the resource
//!    TYPE declares each property in — the inverse of the translation the create applied — so a
//!    stack whose resources are as the deployment left them is reported `IN_SYNC`. An
//!    `AWS::AutoScaling::AutoScalingGroup` is where the two sides differ: the type declares
//!    `VPCZoneIdentifier` as a LIST of subnet ids where the API takes and reports "one or more
//!    comma-separated subnet IDs", and the type's `Cooldown` is that API's `DefaultCooldown`.
//! 2. A resource whose removal the owning service has only ACCEPTED is still there, so what it
//!    depends on is left alone until that service no longer holds it. `DeleteAutoScalingGroup`
//!    terminates the group's instances as it answers, and each instance holds the subnet it was
//!    placed in until Amazon EC2 reports it `terminated` — so a stack that launched instances
//!    into its own VPC tears down completely instead of failing on the subnet its own instances
//!    were still in.
//!
//! The rows differ along what the rules generalise over: how the group states where it launches
//! (zones named outright, the subnets of a VPC the same stack creates), whether it carries a
//! propagated tag, and whether it holds an instance at all.

use smithy_server_runtime::MultiServiceRouter;

use crate::harness::stack_event_reasons;
use crate::stack_lifecycle::{
    call, create_stack, list_members, member, router, settled_stack, xml_blocks, xml_text, REGION,
};

/// A group that names its zones outright, holds no instance, and carries the scaling policy a
/// client attaches to one: the shape whose every declared property the owning service reports
/// back under the same name.
const ZONE_PLACED_GROUP: &str = concat!(
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
    "      Cooldown: \"120\"\n",
    "      AvailabilityZones:\n",
    "        - us-east-1a\n",
    "      LaunchTemplate:\n",
    "        LaunchTemplateId: !Ref Lt\n",
    "        Version: !GetAtt Lt.LatestVersionNumber\n",
    "  Policy:\n",
    "    Type: AWS::AutoScaling::ScalingPolicy\n",
    "    Properties:\n",
    "      AutoScalingGroupName: !Ref Asg\n",
    "      PolicyType: TargetTrackingScaling\n",
    "      TargetTrackingConfiguration:\n",
    "        PredefinedMetricSpecification:\n",
    "          PredefinedMetricType: ASGAverageCPUUtilization\n",
    "        TargetValue: 50\n",
    "Outputs:\n",
    "  GroupName:\n",
    "    Value: !Ref Asg\n",
);

/// The same estate as a client deploys it: the group places its instances in the SUBNETS of a
/// VPC the same stack creates, carries a tag it propagates to them, and launches one — so its
/// `VPCZoneIdentifier` is the property the two sides spell differently, and its instance is what
/// the teardown has to wait for.
const SUBNET_PLACED_ESTATE: &str = concat!(
    "Resources:\n",
    "  Vpc:\n",
    "    Type: AWS::EC2::VPC\n",
    "    Properties:\n",
    "      CidrBlock: 10.149.0.0/16\n",
    "  SubnetA:\n",
    "    Type: AWS::EC2::Subnet\n",
    "    Properties:\n",
    "      VpcId: !Ref Vpc\n",
    "      CidrBlock: 10.149.1.0/24\n",
    "      AvailabilityZone: !Select [0, !GetAZs \"\"]\n",
    "  SubnetB:\n",
    "    Type: AWS::EC2::Subnet\n",
    "    Properties:\n",
    "      VpcId: !Ref Vpc\n",
    "      CidrBlock: 10.149.2.0/24\n",
    "      AvailabilityZone: !Select [1, !GetAZs \"\"]\n",
    "  Lt:\n",
    "    Type: AWS::EC2::LaunchTemplate\n",
    "    Properties:\n",
    "      LaunchTemplateData:\n",
    "        ImageId: ami-00000000000000001\n",
    "        InstanceType: t3.micro\n",
    "  Asg:\n",
    "    Type: AWS::AutoScaling::AutoScalingGroup\n",
    "    Properties:\n",
    "      MinSize: \"1\"\n",
    "      MaxSize: \"2\"\n",
    "      DesiredCapacity: \"1\"\n",
    "      Cooldown: \"300\"\n",
    "      VPCZoneIdentifier:\n",
    "        - !Ref SubnetA\n",
    "        - !Ref SubnetB\n",
    "      Tags:\n",
    "        - Key: Name\n",
    "          Value: deployed-fleet\n",
    "          PropagateAtLaunch: true\n",
    "      LaunchTemplate:\n",
    "        LaunchTemplateId: !Ref Lt\n",
    "        Version: !GetAtt Lt.LatestVersionNumber\n",
    "  Policy:\n",
    "    Type: AWS::AutoScaling::ScalingPolicy\n",
    "    Properties:\n",
    "      AutoScalingGroupName: !Ref Asg\n",
    "      PolicyType: TargetTrackingScaling\n",
    "      TargetTrackingConfiguration:\n",
    "        PredefinedMetricSpecification:\n",
    "          PredefinedMetricType: ASGAverageCPUUtilization\n",
    "        TargetValue: 50\n",
    "Outputs:\n",
    "  GroupName:\n",
    "    Value: !Ref Asg\n",
);

/// RULE 1: a stack whose resources are as the deployment left them is reported `IN_SYNC`, with
/// no resource of it reported drifted — so a client probing a freshly deployed autoscaling
/// estate for drift is told the truth about it.
#[tokio::test]
async fn a_freshly_deployed_group_reports_no_drift() {
    for (case, stack, template) in [
        (
            "a group that names its zones outright",
            "asg-fleet-drift-zones",
            ZONE_PLACED_GROUP,
        ),
        (
            "a group placed in the subnets of its own VPC, with a propagated tag",
            "asg-fleet-drift-subnets",
            SUBNET_PLACED_ESTATE,
        ),
    ] {
        let router = router();
        deployed(&router, case, stack, template).await;

        let (status, started) =
            call(&router, "DetectStackDrift", &[member("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DetectStackDrift must answer 200, got {status} {started}"
        );
        let detection = xml_text(&started, "StackDriftDetectionId").unwrap_or_default();
        let concluded = settled_detection(&router, case, &detection).await;
        let per_resource = reported_drifts(&router, case, stack).await;
        assert_eq!(
            xml_text(&concluded, "StackDriftStatus").as_deref(),
            Some("IN_SYNC"),
            "case {case:?}: expected no drift before any out-of-band change, got {concluded} \
             with {per_resource:?}"
        );
    }
}

/// RULE 2: the stack's own deletion COMPLETES, whether the group was holding instances or not —
/// the group's removal is finished only once the instances it terminated are gone from Amazon
/// EC2, so the subnet they were placed in is deleted after them rather than refused for them.
#[tokio::test]
async fn a_deployed_group_tears_its_whole_estate_down() {
    for (case, stack, template) in [
        (
            "a group holding no instance",
            "asg-fleet-teardown-zones",
            ZONE_PLACED_GROUP,
        ),
        (
            "a group holding an instance, in the subnets of its own VPC",
            "asg-fleet-teardown-subnets",
            SUBNET_PLACED_ESTATE,
        ),
    ] {
        let router = router();
        deployed(&router, case, stack, template).await;

        let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must answer 200, got {status} {body}"
        );
        let settled = settled_stack(&router, case, stack).await;
        assert!(
            settled.is_empty()
                || xml_text(&settled, "StackStatus").as_deref() == Some("DELETE_COMPLETE"),
            "case {case:?}: expected the delete to complete, got {settled} with reasons {:?}",
            stack_event_reasons(&router, REGION, stack).await
        );
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
    // The group the stack published, which is the handle every read of it takes.
    let outputs = xml_blocks(&described, "Outputs")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert!(
        xml_blocks(&outputs, "member")
            .into_iter()
            .any(|entry| xml_text(&entry, "OutputKey").as_deref() == Some("GroupName")),
        "case {case:?}: expected the stack to publish its group's name, got {described}"
    );
    described
}

/// Poll `DescribeStackDriftDetectionStatus` the way a client's waiter does and answer the body
/// of the first read that reports the detection no longer in progress.
///
/// A detection's result is due at an INSTANT this service records, so what this waits for is
/// the CONDITION — a `DetectionStatus` that is not `DETECTION_IN_PROGRESS` — under a deadline
/// far longer than the span the emulator gives one.
async fn settled_detection(
    router: &MultiServiceRouter,
    case: &str,
    detection_id: &str,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (status, body) = call(
            router,
            "DescribeStackDriftDetectionStatus",
            &[member("StackDriftDetectionId", detection_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackDriftDetectionStatus must answer 200, got {status} \
             {body}"
        );
        let reported = xml_text(&body, "DetectionStatus").unwrap_or_default();
        if reported != "DETECTION_IN_PROGRESS" {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the detection to conclude, it is still {reported:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// What the detection reported about each resource of the stack: its logical id, its drift
/// status and the property differences it carries — which is what names the resource a stack
/// was reported drifted for.
async fn reported_drifts(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, Vec<String>)> {
    let (status, body) = call(
        router,
        "DescribeStackResourceDrifts",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResourceDrifts must answer 200, got {status} {body}"
    );
    list_members(&body, "StackResourceDrifts")
        .into_iter()
        .map(|entry| {
            (
                xml_text(&entry, "LogicalResourceId").unwrap_or_default(),
                xml_text(&entry, "StackResourceDriftStatus").unwrap_or_default(),
                list_members(&entry, "PropertyDifferences")
                    .into_iter()
                    .map(|difference| {
                        format!(
                            "{}: expected {:?}, actual {:?}",
                            xml_text(&difference, "PropertyPath").unwrap_or_default(),
                            xml_text(&difference, "ExpectedValue").unwrap_or_default(),
                            xml_text(&difference, "ActualValue").unwrap_or_default(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}
