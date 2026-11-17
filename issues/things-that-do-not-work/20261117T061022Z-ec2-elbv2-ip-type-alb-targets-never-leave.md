Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elbv2.albos-ip-targets-a-listener-stops-routing-to-revert-to-unused-not-in-use ec2__elbv2.albos-stale-ip-targets-behind-a-listener-fail-health-checks-while-an-unrouted-twin-stays-unused
Issue-Key: ec2__elbv2.albos-ip-targets-a-listener-stops-routing-to-revert-to-unused-not-in-use|_poll#11|Timeout|time.sleep(interval_s)
Issue-Key: ec2__elbv2.albos-stale-ip-targets-behind-a-listener-fail-health-checks-while-an-unrouted-twin-stays-unused|_poll#13|Timeout|time.sleep(interval_s)
Issue-Commit: 1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61

# ec2__elbv2: ip-type ALB targets never leave 'initial' — unreachable addresses behind a listener never settle 'unhealthy', and they never revert to 'unused'/'Target.NotInUse' when no listener forwards to the group

> _Note: this issue was identified against the code base at commit `1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
Cause 1 of 1: ELBv2 target health for ip-type targets never converges, in either direction.

AWS: recorded behavior only (the Smithy model types TargetHealth.State as an enum of initial/healthy/unhealthy/unused/draining/unavailable with a Reason, but states no transition rule). What the recordings show: targets registered into a target group that NO listener forwards to read State 'unused' with Reason 'Target.NotInUse'. Once a listener (or a listener rule) forwards to that group, the same targets leave 'initial' and — for addresses inside the VPC CIDR that are attached to nothing, so every HTTPS/443 health probe fails — settle on State 'unhealthy' after UnhealthyThresholdCount consecutive failures; the Reason is not stable across groups (one recording read Target.Timeout in one group and Target.FailedHealthChecks in another), so only the State is a requirement. Deleting the only listener that forwards there flips those same targets back to 'unused'/'Target.NotInUse': the state tracks the group's CURRENT wiring, not a health verdict latched at the last probe. A load balancer leaves State.Code 'provisioning' for 'active' on the way there.

Emulator: both checks are cut off at the harness's 60 s budget with no verdict (their own ceilings are 600 s / 180 s) while polling DescribeTargetHealth for every target to have left 'initial'. The ip targets stay in 'initial' indefinitely: they never settle 'unhealthy' although no probe can succeed against them, and consequently the revert half (delete the listener, expect 'unused'/'Target.NotInUse') and the unrouted-twin half (a third group holding the same addresses that must read 'unused'/'Target.NotInUse' at the same instant) are never reached either. Hypothesis: there is no health-probe driver and no wiring-derived state for ip-type targets, so the registration-time 'initial' is terminal.

Why: DescribeTargetHealth is the only signal a client has for whether an ALB will route to a target. A target group permanently in 'initial' makes every deployment gate, every "is my blue/green cutover safe" query and every stale-target cleanup unanswerable: a client waiting for 'healthy' or 'unhealthy' waits forever, and one that treats 'initial' as transient-but-fine routes to addresses that answer nothing.

Fix: the ELBv2 target-health state behind DescribeTargetHealth — derive the state from the group's current wiring (no listener/rule forwarding to the group ⇒ 'unused' with Reason 'Target.NotInUse') and from the configured health check (HealthCheckProtocol/Port/IntervalSeconds/TimeoutSeconds, UnhealthyThresholdCount/HealthyThresholdCount, Matcher) for a routed group, so an unreachable ip target transitions initial → unhealthy and a group that loses its last listener transitions its targets back to unused.

Traces: ec2__elbv2.albos-ip-targets-a-listener-stops-routing-to-revert-to-unused-not-in-use, ec2__elbv2.albos-stale-ip-targets-behind-a-listener-fail-health-checks-while-an-unrouted-twin-stays-unused.

Neither check is marked owned or stub-blocked in the replay report. Open issues on neighbouring target-health symptoms exist and do not cover these replay keys: 20261111T030957Z-autoscaling-ec2-elbv2-4-net-new-causes-target.md (ASG/EC2-instance targets synthesized 'healthy'), 20261109T153103Z-elbv2-4-net-new-causes-the-published-dnsname.md, and 20261115T034057Z-ec2-elbv2-reopens-the-alb-still-announces-an.md.

## Evidence
ec2__elbv2.albos-ip-targets-a-listener-stops-routing-to-revert-to-unused-not-in-use [mismatch]:
  Error: Check timed out after 60.0s
  The check registers two VPC-internal addresses attached to nothing as ip targets on port 443, creates an internal ALB and an HTTP:80 listener forwarding to the group, then polls DescribeTargetHealth (ceiling 600 s) until every target has left 'initial'. Expected (AWS): both targets → State 'unhealthy'; then after DeleteListener, both → ('unused', 'Target.NotInUse'). Actual: the targets never leave 'initial', so the first poll never completes.

ec2__elbv2.albos-stale-ip-targets-behind-a-listener-fail-health-checks-while-an-unrouted-twin-stays-unused [mismatch]:
  Error: Check timed out after 60.0s
  Three target groups hold the same two unreachable ip targets; a listener default action forwards to the first and a path-pattern rule forwards to the second, the third is referenced by nothing. The check polls the ALB out of 'provisioning' (expect 'active') and then polls both routed groups' health (ceiling 600 s) until no target reads 'initial'. Expected (AWS): both routed groups → every target 'unhealthy' (Reason not asserted: Target.Timeout and Target.FailedHealthChecks both observed in real recordings), and at the same instant the unrouted twin → ('unused', 'Target.NotInUse') for both addresses. Actual: no target ever leaves 'initial'.

Health-check configuration used by both (so the probe cadence is not in question): HealthCheckEnabled true, Protocol HTTPS, Port 443, IntervalSeconds 30, TimeoutSeconds 10, HealthyThresholdCount 2, UnhealthyThresholdCount 3, Matcher HttpCode '200,301,302'.

## Steps to Reproduce
1. Create a VPC with three /22 subnets across two AZs (no IGW, no NAT) and a security group.
2. CreateTargetGroup with TargetType ip in that VPC, health check HTTPS:443, interval 30, timeout 10, healthy 2, unhealthy 3, matcher '200,301,302'.
3. RegisterTargets with two addresses inside the VPC CIDR that are attached to no ENI, port 443.
4. CreateLoadBalancer (internal) across two of the subnets, then CreateListener Port 80 Protocol HTTP with a default forward action to the group.
5. Poll DescribeTargetHealth on the group: the targets must leave 'initial' and settle State 'unhealthy'.
6. DeleteListener, then poll DescribeTargetHealth again: both targets must read State 'unused' with Reason 'Target.NotInUse'.
7. For the second check, add a third target group holding the same two addresses that no listener or rule references and assert it reads ('unused','Target.NotInUse') while the two routed groups read 'unhealthy'.
