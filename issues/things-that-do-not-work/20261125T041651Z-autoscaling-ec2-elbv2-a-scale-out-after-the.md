Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted
Issue-Key: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted|run#24|Timeout|time.sleep(stall_interval_s)
Issue-Commit: 36c0ec5dd403fd4e9beb4ab000e55c4d6e6ab9a1

# autoscaling__ec2__elbv2: a scale-out after the attached target group is deleted out-of-band never settles — no Failed scaling activity naming the missing target group, and the detach/attach restore never converges

> _Note: this issue was identified against the code base at commit `36c0ec5dd403fd4e9beb4ab000e55c4d6e6ab9a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Cause 1 of 1: an Auto Scaling group whose only attached ELBv2 target group has been deleted out of band must fail its next launch and say why; in the emulator the whole sequence never reaches a settled state and the check is cut off by the replay budget. Failing check id: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted.

AWS: Auto Scaling re-validates every attached traffic source on each launch. Smithy (autoscaling): SetDesiredCapacity, DescribeScalingActivities -> Activity.StatusCode / Activity.StatusMessage; AttachLoadBalancerTargetGroups / DetachLoadBalancerTargetGroups; elbv2 DescribeTargetHealth -> TargetHealthDescriptions[].Target.Id. Beyond the shapes this is recorded/documented behaviour only: after a scale-out to a live target group has registered the group's instances, deleting that target group with elbv2 DeleteTargetGroup and then raising DesiredCapacity again produces a scaling activity whose StatusCode is the fixed string "Failed" and whose StatusMessage carries AWS's fixed text for this class, "One or more target groups not found", with the variable part being the ARN(s) of the still-attached target groups taken from the group's own attachment records. No instance is launched by that activity: the group's Instances list stays at the count it had before, and it stays there for as long as the request is retried. Once DetachLoadBalancerTargetGroups removes the deleted ARN and AttachLoadBalancerTargetGroups adds a live one, the next SetDesiredCapacity is honoured — desired capacity is met, and the freshly launched instance appears in DescribeTargetHealth for the new target group.

Emulator: the replay never gets a settled answer out of this sequence. The check's own polls (180 s for target-group registration, 240 s for the Failed activity, 600 s for the restore) are all still unsatisfied when the harness cuts the check off: the only detail recorded is `Error: Check timed out after 60.0s`, with no Failed activity, no stalled-at-two-instances observation and no restored third instance ever observed. So at least one of these never happens: ASG instances reaching InService and registering in the attached target group, the deleted target group being re-validated at launch time so the activity ends Failed with a message naming it, or the detach-of-the-dead-plus-attach-of-the-live target group lifting the block.

Why: a client that uses a target group as the gate on a group's launches gets no signal at all. It cannot learn from DescribeScalingActivities that its scale-out was refused (no Failed activity, no 'target group' message), cannot rely on the group staying at its old size, and cannot tell when repairing the attachment has unblocked capacity — a deployment or self-healing script that waits for either verdict waits forever instead of failing fast with AWS's refusal text.

Fix: the Auto Scaling launch path's traffic-source validation and the scaling-activity recorder (validate each attached target-group ARN against elbv2 state before launching, record a Failed activity with the 'One or more target groups not found' message and launch nothing), plus the target-group registration path that must make ASG instances visible in DescribeTargetHealth and the attach/detach handlers that must make the group's attachment set authoritative for that validation.

Related open issues that do NOT cover this check (the replay did not mark it owned by any of them): 20261107T075601Z-autoscaling-ec2-elbv2-auto-scaling-elbv2-asg-instances-never-register-with.md (ASG instances never register with target groups), 20261122T103325Z-autoscaling-ec2-elbv2-a-single-detachloadbalancertargetgroups-never-clears.md (a single DetachLoadBalancerTargetGroups does not clear every view) and 20261117T012829Z-autoscaling-ec2-elbv2-3-net-new-causes-the.md (separate attachment stores). If those fixes land, re-replay this check: it asserts the refusal-on-launch behaviour none of them states.

Traces: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted (verdict: mismatch).

## Evidence
Expected (AWS): after elbv2 DeleteTargetGroup on the group's only attached target group, SetDesiredCapacity(DesiredCapacity=3, HonorCooldown=False) yields a DescribeScalingActivities entry with StatusCode == "Failed" and a StatusMessage containing AWS's fixed text "One or more target groups not found" (variable part: the attached target-group ARN), the group's Instances list stays at 2 for the whole watch window, and after DetachLoadBalancerTargetGroups(dead ARN) + AttachLoadBalancerTargetGroups(live ARN) the group reaches 3 InService instances and the new instance id appears in DescribeTargetHealth(TargetGroupArn=<new tg>).

Actual (emulator): no such observation is ever made. The replay detail is exactly:
  Error: Check timed out after 60.0s
No Failed scaling activity, no 'target group' message, no restored third instance and no registration of the new instance in the replacement target group were recorded before the budget expired; every internal poll in the check (180 s / 240 s / 600 s) was still unsatisfied.

## Steps to Reproduce
1. In the shared VPC, create a launch template (latest AL2 AMI, t3.micro, default SG) and an ELBv2 target group TG1 (protocol TCP, port 22) in that VPC.
2. CreateAutoScalingGroup with that launch template, min=1 max=3 desired=1, TargetGroupARNs=[TG1], HealthCheckType=EC2; wait for 1 InService instance.
3. SetDesiredCapacity(DesiredCapacity=2, HonorCooldown=False); wait for 2 InService instances and confirm both appear in DescribeTargetHealth(TargetGroupArn=TG1).
4. elbv2 DeleteTargetGroup(TargetGroupArn=TG1).
5. SetDesiredCapacity(DesiredCapacity=3, HonorCooldown=False) and poll DescribeScalingActivities for a StatusCode=="Failed" activity whose StatusMessage mentions the target group. Expected: such an activity appears and DescribeAutoScalingGroups keeps showing 2 instances. Observed: nothing settles.
6. AttachLoadBalancerTargetGroups(TG2) on a freshly created target group, DetachLoadBalancerTargetGroups(TG1), SetDesiredCapacity(3) and poll for 3 InService instances and the new instance in DescribeTargetHealth(TG2). Expected: both hold. Observed: the check is still waiting when the replay budget expires (`Check timed out after 60.0s`).
