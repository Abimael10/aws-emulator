Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks|run#22|AssertionError|assert last_states[0]["state"] == "unhealthy", f"unexpected terminal state: {last_states}"
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# cloudwatch__ecs__elbv2__logs: a target the ALB cannot reach is never reported unhealthy — the group churns between a draining and an initial target instead of reaching 'unhealthy'/Target.Timeout

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
Cause 1 of 1: with an ECS task security group that has no ingress rule from the ALB's security group, the ALB's health check cannot reach the task, but DescribeTargetHealth never reports the target 'unhealthy'; instead the group keeps presenting a previous target in 'draining' (Reason 'Target.DeregistrationInProgress') and a fresh one in 'initial' (Reason 'Elb.RegistrationInProgress'), for a service whose desiredCount is 1.
AWS:      Smithy, elasticloadbalancingv2 TargetHealthStateEnum = [initial, healthy, unhealthy, unhealthy.draining, unused, draining, unavailable] and TargetHealth.Reason — "If the target state is unhealthy, the reason code can be one of the following values: ... Target.Timeout - The health check requests timed out" (also Target.FailedHealthChecks for a connection error). A registered ip target whose health-check port is unreachable because the security group denies the load balancer leaves 'initial'/Elb.InitialHealthChecking and settles at State 'unhealthy' with Reason 'Target.Timeout' (Description "Request timed out") after UnhealthyThresholdCount consecutive failed probes, and it STAYS registered — the ECS service does not replace a task for a failing ELB health check once it is the only task and the deployment cannot make progress, so the group holds exactly one entry for a desiredCount=1 service. The ALB answers 503 for the group because no target is healthy.
Emulator: over the whole 120 s monitoring window DescribeTargetHealth never reports 'unhealthy'. The terminal observation is two entries for a desiredCount=1 service: [{'id': '<ip-1>', 'state': 'draining', 'reason': 'Target.DeregistrationInProgress'}, {'id': '<ip-2>', 'state': 'initial', 'reason': 'Elb.RegistrationInProgress'}] — i.e. the task is being replaced and the registration restarted, so the target set oscillates between draining and initial and no health verdict is ever produced. (The ip values are per-run and are not part of the requirement; the states and reason codes are.)
Why:      a security group that blocks the load balancer is one of the most common production misconfigurations, and the only signal AWS gives for it is TargetHealth.State 'unhealthy' with Reason 'Target.Timeout'. A client (or an operator, or an alarm on UnHealthyHostCount) that relies on that signal sees a target group that looks like it is merely still registering, forever, and cannot distinguish "blocked by a security group" from "not yet registered". Reporting an extra draining target for a desiredCount=1 service also misleads any capacity or drain-completion logic.
Fix:      the ELBv2 target-health evaluator (it must drive a target whose probe cannot be delivered to 'unhealthy' with Reason 'Target.Timeout' after the configured UnhealthyThresholdCount, instead of re-registering it), and the ECS service scheduler's decision to replace a task whose ELB health check fails while it is the service's only task.
Nearest open issue: 20261105T204413Z-cloudwatch-ecs-elbv2-logs-an-unreachable-ip-target.md describes an unreachable target that never leaves 'initial'; it does not carry this check's replay key and does not describe the draining/initial churn seen here.
Traces:   cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks.

## Evidence
#### [mismatch] cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks
  File "check_cloudwatch__ecs__elbv2__logs_alb_security_group_mismatch_blocks_health_checks.py", line 81, in run
    assert last_states[0]["state"] == "unhealthy", f"unexpected terminal state: {last_states}"
  AssertionError: unexpected terminal state: [{'id': '172.31.0.5', 'state': 'draining', 'reason': 'Target.DeregistrationInProgress'}, {'id': '172.31.0.6', 'state': 'initial', 'reason': 'Elb.RegistrationInProgress'}]

Expected: DescribeTargetHealth reports one TargetHealthDescription for the desiredCount=1 service with TargetHealth.State 'unhealthy' and Reason 'Target.Timeout' (never 'healthy'), and the ALB never answers 200.
Actual: no entry ever reaches 'unhealthy'; the group holds a draining entry (Target.DeregistrationInProgress) plus a newly registering one (Elb.RegistrationInProgress) for the whole window.

## Steps to Reproduce
1. In a VPC with two subnets create two security groups: alb-sg with tcp/80 ingress from the client CIDRs, and task-sg with NO ingress rules at all.
2. CreateLoadBalancer (internet-facing ALB, alb-sg), CreateTargetGroup (TargetType ip, protocol HTTP, port 80, HealthCheckPath '/') and CreateListener forwarding to it.
3. RegisterTaskDefinition (awsvpc, nginx serving a marker on port 80) and ecs CreateService with desiredCount 1, awsvpcConfiguration using task-sg, and loadBalancers wired to the target group; wait for runningCount 1.
4. Poll elbv2 DescribeTargetHealth for up to 120 s and GET http://<alb-dns>/ each round.
5. Observe no entry ever reports State 'unhealthy' with Reason 'Target.Timeout'; instead the group alternates a 'draining'/Target.DeregistrationInProgress entry with an 'initial'/Elb.RegistrationInProgress entry.
