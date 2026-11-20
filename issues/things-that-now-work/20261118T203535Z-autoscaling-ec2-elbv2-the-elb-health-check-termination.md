Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elbv2.asghc-elb-type-group-replaces-its-unhealthy-instance-while-an-ec2-type-group-keeps-its-own
Issue-Key: autoscaling__ec2__elbv2.asghc-elb-type-group-replaces-its-unhealthy-instance-while-an-ec2-type-group-keeps-its-own|run#16|AssertionError|assert "in response to an ELB system health check failure" in term[0]["Cause"], (
Issue-Commit: e1f71a84fd686660699f460f533bddedc75c2265

# autoscaling__ec2__elbv2: the ELB-health-check termination activity's Cause reads "a ELB system health check failure" instead of AWS's "an ELB system health check failure" (1 cause)

> _Note: this issue was identified against the code base at commit `e1f71a84fd686660699f460f533bddedc75c2265`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
Cause 1 of 1: the scaling activity an HealthCheckType=ELB group records when it terminates an instance its target group's health check failed carries a re-worded Cause. Failing check id: autoscaling__ec2__elbv2.asghc-elb-type-group-replaces-its-unhealthy-instance-while-an-ec2-type-group-keeps-its-own.

AWS: recorded behavior only — the Smithy model says nothing about this text (`Activity$Cause` is documented just as "The reason the activity began.", a free-form string). The recording shows that for a termination an Elastic Load Balancing health-check verdict caused, the Cause of the "Terminating EC2 instance: <id>" activity contains the fixed phrase "an instance was taken out of service in response to an ELB system health check failure." The only variable parts of that sentence are the leading "At <timestamp>" instant and the instance id (which is carried in Activity$Description); the phrase itself is fixed text, and it is what distinguishes an ELB verdict from the EC2-health or user-request wordings.

Emulator: DescribeScalingActivities serves the Cause as 'an instance was taken out of service in response to a ELB system health check failure.' — the article in the fixed text is "a" where AWS writes "an ELB system health check failure". Everything else about the activity matched: exactly one termination activity, naming the ELB-type group's first instance, while the EC2-type group attached to the same target group kept its instance.

Why: the Cause phrase is the documented, fixed way a client tells WHY a member was replaced. Runbooks, log scrapers, alerting rules and tests that match AWS's phrase (as this check does, with a substring test for "in response to an ELB system health check failure") see no match against the emulator, so an ELB-driven replacement is read as an unclassified one.

Fix: the cause-text constant used when the group's Elastic Load Balancing health reconcile records the terminate activity (the ELB-health path in services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/state, the module that records the "Terminating EC2 instance" activity for an ELB verdict) — its fixed text must read "an ELB system health check failure".

Not covered by any open issue: 20261117T120522Z-autoscaling-ecs-elbv2-an-asg-with-healthchecktype-elb.md is about an ELB-type group never reporting an instance Unhealthy at all, under a different service token, and here the group DOES terminate and record the activity — only its wording diverges. No open issue carries this check's replay key.

Traces: autoscaling__ec2__elbv2.asghc-elb-type-group-replaces-its-unhealthy-instance-while-an-ec2-type-group-keeps-its-own.

## Evidence
Expected (recorded AWS): DescribeScalingActivities for the HealthCheckType=ELB group returns one activity with Description starting "Terminating EC2 instance: <first-instance-id>" and a Cause containing the fixed phrase "an instance was taken out of service in response to an ELB system health check failure."
Actual (emulator): the same activity's Cause is 'an instance was taken out of service in response to a ELB system health check failure.' — "a ELB" instead of "an ELB".
Check assertion that fails: assert "in response to an ELB system health check failure" in term[0]["Cause"] -> AssertionError: the termination's recorded cause: 'an instance was taken out of service in response to a ELB system health check failure.'

## Steps to Reproduce
1. Create a VPC/subnet, a security group with no ingress to the health-check port, a launch template, an NLB, and a target group (HTTP health check, interval 10 s, timeout 5 s, UnhealthyThresholdCount 2) with a listener forwarding to it.
2. Create two ASGs on the same launch template and subnet, both attached to that target group, Min=Max=Desired=1, HealthCheckGracePeriod=30: one with HealthCheckType=ELB, one with HealthCheckType=EC2.
3. Wait until each group has one InService instance; both targets fail the target group's health check.
4. Poll DescribeScalingActivities on the ELB-type group until an activity whose Description starts "Terminating EC2 instance" appears, and read its Cause.
5. The Cause contains "in response to a ELB system health check failure" instead of AWS's "in response to an ELB system health check failure".
Replay: autoscaling__ec2__elbv2.asghc-elb-type-group-replaces-its-unhealthy-instance-while-an-ec2-type-group-keeps-its-own
