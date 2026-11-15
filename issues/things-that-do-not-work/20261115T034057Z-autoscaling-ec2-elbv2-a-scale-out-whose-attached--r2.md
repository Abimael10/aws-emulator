Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted
Issue-Key: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted|run#24|Timeout|time.sleep(stall_interval_s)
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# autoscaling__ec2__elbv2: a scale-out whose attached target group was deleted never produces a Failed activity or a converged group, so the block and its recovery can never be observed

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
=== Cause 1 of 1: the deleted-target-group launch block never materialises and the group never reaches desired capacity, so the sequence never terminates ===
AWS: recorded behavior only. A group with an attached target group scales out normally and its instances register in that target group. After the target group is deleted out of band, the next SetDesiredCapacity is accepted but the launch fails: DescribeScalingActivities reports an activity with StatusCode `Failed` whose status message names the missing target group (AWS words this class as "one or more target groups not found"), the group stays at its previous instance count, and no launch completes later. Attaching a fresh target group and detaching the deleted one lifts the block, the group reaches the new desired capacity and the newly launched instance registers in the fresh target group. Every one of those states is readable within a client's polling budget.
Emulator: none of it is observable — the check is killed with `Check timed out after 60.0s` without reaching a verdict, so no Failed activity naming the target group is produced, the group's convergence after the swap is never seen, and the registration of the restored instance never appears.
Why: a deleted target group is one of the commonest real causes of an Auto Scaling group that silently stops launching. A client cannot diagnose it (there is no Failed activity to read), cannot confirm the group is blocked rather than merely slow, and cannot verify that swapping in a new target group recovers it. Related open issues record other facets of ASG/target-group registration (e.g. 20261107T075601Z-autoscaling-ec2-elbv2-auto-scaling-elbv2-asg-instances-never-register-with.md, 20261112T140934Z-autoscaling-ec2-elbv2-autoscaling-autoscaling-ec2-elbv2-4-net-new-causes.md); neither carries this check's key.
Fix: the launch path of the autoscaling service must validate every attached target group before launching, record a `Failed` scaling activity whose message names the missing target group, hold the group at its current capacity, and converge once the attachments are repaired — and the elbv2 registration of the launched instance must follow.
Traces: autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted.

## Evidence
autoscaling__ec2__elbv2.asg-scale-up-blocked-after-target-group-deleted: `Error: Check timed out after 60.0s` — killed with no assertion reached. Expected, in order: a control SetDesiredCapacity to 2 reaching 2 InService instances registered in the target group; after DeleteTargetGroup, a SetDesiredCapacity to 3 followed by a DescribeScalingActivities entry with StatusCode='Failed' whose message names the target group, with the group held at 2 instances; after attaching a fresh target group and detaching the deleted one, 3 InService instances with the new one registered in the fresh target group.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Create a launch template and a target group; create an Auto Scaling group (min 1, max 3, desired 1) with that TargetGroupARN and HealthCheckType=EC2; wait for 1 InService instance.
3. SetDesiredCapacity=2 and wait for both instances to register in the target group.
4. DeleteTargetGroup on the attached group, then SetDesiredCapacity=3 and poll DescribeScalingActivities — no Failed activity naming the target group appears.
5. Create a second target group, AttachLoadBalancerTargetGroups it, DetachLoadBalancerTargetGroups the deleted one, SetDesiredCapacity=3 again and poll — the group never reaches 3 InService with the new instance registered.
