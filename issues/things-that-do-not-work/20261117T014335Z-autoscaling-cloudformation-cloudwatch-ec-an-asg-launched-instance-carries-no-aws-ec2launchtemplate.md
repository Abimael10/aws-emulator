Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation__cloudwatch__ec2.deploy-autoscaling-3-cdk-l2-asg-target-tracking-deploys-runs-and-drifts
Issue-Key: autoscaling__cloudformation__cloudwatch__ec2.deploy-autoscaling-3-cdk-l2-asg-target-tracking-deploys-runs-and-drifts|run#38|AssertionError|assert itags.get("aws:ec2launchtemplate:id") == lt_id, (
Issue-Commit: 4f5988ed583bd7215984a8da4cf24484af3f613a

# An ASG-launched instance carries no aws:ec2launchtemplate:id tag, so a CDK L2 target-tracking ASG deploy cannot be traced back to the stack's launch template

> _Note: this issue was identified against the code base at commit `4f5988ed583bd7215984a8da4cf24484af3f613a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
One net-new root cause, grounded in one failing behavioral check.

Cause 1 of 1: an EC2 instance launched by an Auto Scaling group whose capacity comes from an EC2 launch template is not stamped with the AWS-reserved launch-template identity tag aws:ec2launchtemplate:id (and, by the same path, aws:ec2launchtemplate:version). DescribeInstances reports the instance with that tag absent.

AWS: recorded behavior only for the reserved tag itself — the Smithy models do not model AWS-stamped system tags. Smithy (ec2): com.amazonaws.ec2#LaunchTemplateSpecification has members LaunchTemplateId ("The ID of the launch template"), LaunchTemplateName and Version ("The launch template version number, $Latest, or $Default"); com.amazonaws.ec2#TagSpecification carries the tags applied to a resource at creation. What the recording shows: when Auto Scaling launches capacity for a group bound to a launch template, EC2 stamps the launched instance with the reserved tags aws:ec2launchtemplate:id, whose value is the id of the launch template the group resolved (a value taken from the group's LaunchTemplateSpecification and EC2's launch-template state, never from the check's request), and aws:ec2launchtemplate:version, whose value is the RESOLVED version number as a decimal string. These sit alongside aws:autoscaling:groupName and the group's PropagateAtLaunch tags on the same instance. The call succeeds (HTTP 200) in both AWS and the emulator; the divergence is in the Tags list of the instance, not in a status or an error code.

Emulator: the launched instance's Tags contain aws:autoscaling:groupName (that assertion passes immediately before) but no aws:ec2launchtemplate:id at all — the lookup yields None where AWS yields the stack's launch-template id. Replay detail: AssertionError "instance '<instance-id>' carries aws:ec2launchtemplate:id None, expected the stack's '<lt-id>'" (the two ids are per-run values; the requirement is that the key is present and equals the id of the launch template the group launched from).

Why: a client cannot join a running instance back to the launch template — nor to the template VERSION — that produced it. Tag-driven inventory, cost allocation, tag-filtered DescribeInstances, and CDK/CloudFormation-era rollout verification ("did this instance come from the template version my stack declares?") all read a missing key and conclude the instance has no launch-template provenance. Here it also truncates a whole CDK L2 ASG deploy scenario: the drift, target-tracking-alarm and teardown legs of the check never run.

Fix: the Auto Scaling launch path that materialises instances for a group (the RunInstances call it performs on the group's behalf and the tag merge around it, in the autoscaling crate's scale-out/launch handler and the ec2 crate's instance-tag state) must stamp the reserved aws:ec2launchtemplate:id and aws:ec2launchtemplate:version tags from the group's resolved LaunchTemplateSpecification, in addition to aws:autoscaling:groupName and the propagated group tags it already applies.

Related open issues that do NOT cover this check: 20261108T203125Z-autoscaling-ec2-auto-scaling-group-reads-omit-the-launch-template.md (cause 2) and 20261114T091241Z-autoscaling-ec2-auto-scaling-3-net-new-causes-setdesiredcapacity-above.md describe the same missing-tag symptom, and 20261106T015719Z-autoscaling-ec2-auto-scaling-launch-from-a-launch-template-loses.md describes launch-template data being lost at launch, but none of their replay keys carries this check's id, so this check is not covered by them.

Traces: autoscaling__cloudformation__cloudwatch__ec2.deploy-autoscaling-3-cdk-l2-asg-target-tracking-deploys-runs-and-drifts.

## Evidence
Check [mismatch] autoscaling__cloudformation__cloudwatch__ec2.deploy-autoscaling-3-cdk-l2-asg-target-tracking-deploys-runs-and-drifts

Expected (AWS): after the CDK L2 stack reaches CREATE_COMPLETE and the ASG runs capacity, ec2 DescribeInstances on each InService instance returns Tags containing aws:ec2launchtemplate:id == the id of the launch template the stack created (the CfnOutput LaunchTemplateId), alongside aws:autoscaling:groupName == the group's name and the stack's propagated sim2real tag. AWS also stamps aws:ec2launchtemplate:version with the resolved version number as a decimal string.

Actual (emulator): the instance's Tags carry aws:autoscaling:groupName (the preceding assertion passed) but the aws:ec2launchtemplate:id key is absent, so the lookup returns None:

  File "check_autoscaling__cloudformation__cloudwatch__ec2_deploy_autoscaling_3_cdk_l2_asg_target_tracking_deploys_runs_and_drifts.py", line 247, in run
    assert itags.get("aws:ec2launchtemplate:id") == lt_id, (
  AssertionError: instance 'i-00000000000000001' carries aws:ec2launchtemplate:id None, expected the stack's 'lt-000000000000000b1'

The instance id and launch-template id above are per-run values and are not part of the requirement; the requirement is that the tag key exists on every instance the group launched from a launch template and that its value is that template's id. HTTP status is 200 on both sides — the divergence is purely the missing reserved tag. Because the check aborts here, its later legs (drift IN_SYNC -> DRIFTED on /MaxSize, the two Auto Scaling-created target-tracking alarms in CloudWatch, and the no-instances/no-alarms teardown) were never exercised and carry no verdict.

## Steps to Reproduce
1. ec2 CreateLaunchTemplate(LaunchTemplateName=NAME, LaunchTemplateData={ImageId: <AL2023 ami>, InstanceType: t3.micro, SecurityGroupIds: [SG]}) -> LT_ID. (Equivalently: deploy a CloudFormation/CDK stack declaring AWS::EC2::LaunchTemplate plus AWS::AutoScaling::AutoScalingGroup with a LaunchTemplate reference and a target-tracking policy, as the check does, and read the LaunchTemplateId output.)
2. autoscaling CreateAutoScalingGroup(AutoScalingGroupName=ASG, LaunchTemplate={LaunchTemplateId: LT_ID, Version: "$Latest"}, VPCZoneIdentifier=<public subnets>, MinSize=1, MaxSize=2, DesiredCapacity=1).
3. Poll autoscaling DescribeAutoScalingGroups until an instance reaches LifecycleState InService, then ec2 DescribeInstances on that instance id.
4. Read the instance's Tags.
   Observed: aws:autoscaling:groupName is present, but there is no aws:ec2launchtemplate:id and no aws:ec2launchtemplate:version entry.
   Expected (AWS): aws:ec2launchtemplate:id == LT_ID and aws:ec2launchtemplate:version == the resolved version number as a decimal string ("1" for a fresh template, "2" once a second version exists and the group is on $Latest).
