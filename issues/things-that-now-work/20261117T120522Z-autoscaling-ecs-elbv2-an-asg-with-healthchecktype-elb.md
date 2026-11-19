Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.asg-instance-health-status-derived-from-elb-health-check
Issue-Key: autoscaling__ecs__elbv2.asg-instance-health-status-derived-from-elb-health-check|_wait_for_instance_health#14|TimeoutError|raise TimeoutError(f"instance {instance_id} health {want_status}: "
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# autoscaling__ecs__elbv2: an ASG with HealthCheckType=ELB never reports an instance failing its target group's health check as Unhealthy

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAutoScalingInstances

## Description
Cause 1 of 1: an Auto Scaling group created with HealthCheckType=ELB and an attached ELBv2 target group never derives its instances' health from the target group. An instance that can never pass the target group's HTTP health check (nothing listens on the health-check port) stays HealthStatus=Healthy forever.

AWS: Smithy — Auto Scaling `CreateAutoScalingGroup.HealthCheckType` and `AutoScalingInstanceDetails.HealthStatus` (returned by DescribeAutoScalingInstances); with HealthCheckType=ELB, AWS documents that Auto Scaling additionally consults the attached load balancer's target health and marks the instance Unhealthy once the target group reports it unhealthy (after HealthCheckGracePeriod). The recording asserts exactly that transition: DescribeAutoScalingInstances eventually reports HealthStatus 'Unhealthy' for the instance whose target never answers the probe.

Emulator: the instance never leaves Healthy. The check is cut off with `TimeoutError: instance i-00000000000000001 health unhealthy: the run budget ended first; not graded` — HealthStatus is reported as Healthy for an instance whose target can never pass an HTTP health check on '/'.

Why: a client that relies on HealthCheckType=ELB to replace instances whose application is dead (rather than whose EC2 status check failed) gets no replacement at all. Self-healing groups behind an ALB never recycle a broken instance, and the control loop that watches HealthStatus to decide a rollout failed never fires.

Fix: the Auto Scaling health-status derivation — the group's health evaluation must, when HealthCheckType=ELB, read the attached target group's target health from ELBv2 state and set AutoScalingInstanceDetails.HealthStatus to 'Unhealthy' when the target is unhealthy, rather than reporting the EC2-only verdict.

Traces: autoscaling__ecs__elbv2.asg-instance-health-status-derived-from-elb-health-check.

Related but not covering: 20261107T075601Z-autoscaling-ec2-elbv2-auto-scaling-elbv2-asg-instances-never-register-with.md (registration of ASG instances with target groups, under a different service token) and 20261111T030957Z-autoscaling-ec2-elbv2-4-net-new-causes-target.md (synthesized 'healthy' target health). This cause is specifically the ASG-side HealthStatus never being derived from ELB health.

## Evidence
Expected: DescribeAutoScalingInstances reports HealthStatus 'Unhealthy' for the single instance of a desired-capacity-1 group with HealthCheckType=ELB whose launch template runs no listener, so the target group's HTTP health check on '/' can never succeed.

Actual (replay):
  TimeoutError: instance i-00000000000000001 health unhealthy: the run budget ended first; not graded
  (check_autoscaling__ecs__elbv2_asg_instance_health_status_derived_from_elb_health_check.py line 57 -> _wait_for_instance_health line 344)

The instance id is per-run; the requirement is the Healthy -> Unhealthy transition, not that id.

## Steps to Reproduce
1. Create a VPC with subnets and a security group; create an ELBv2 target group (HTTP, health check path '/') and an internet-facing ALB with a listener forwarding to it; wait for the ALB to become active.
2. Create a launch template with NO user data, so nothing listens on port 80.
3. CreateAutoScalingGroup with DesiredCapacity=1, that launch template, TargetGroupARNs=[tg], HealthCheckType='ELB' and a short HealthCheckGracePeriod.
4. Poll DescribeAutoScalingInstances for the launched instance's HealthStatus.
Observed: HealthStatus stays 'Healthy' indefinitely. Expected: it becomes 'Unhealthy' after the grace period, once the target group reports the target unhealthy.
