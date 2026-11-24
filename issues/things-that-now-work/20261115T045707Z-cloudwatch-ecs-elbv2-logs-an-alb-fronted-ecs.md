Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks cloudwatch__ecs__elbv2__logs.alb-health-check-reflects-ecs-task-health cloudwatch__ecs__elbv2__logs.alb-routes-traffic-only-to-healthy-ecs-targets cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-health-check-reflects-ecs-task-health|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-routes-traffic-only-to-healthy-ecs-targets|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Commit: da3e7884167b3f933d6f1406ab01004ac62fd391

# cloudwatch__ecs__elbv2__logs: an ALB-fronted ECS service never converges to a registered, health-checked target serving its container's bytes — all four target-group-routing checks exhaust their budget without reaching a terminal state

> _Note: this issue was identified against the code base at commit `da3e7884167b3f933d6f1406ab01004ac62fd391`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause with four grounded checks (Cause 1 of 1).

Cause 1 of 1: the canonical ALB + awsvpc ECS service topology never reaches any terminal, observable state in the emulator. Every one of the four target-group-routing checks under this heading is cut off by its own budget ("Check timed out after 60.0s") while polling for the state AWS reaches promptly: an ip target registered for the service's task, that target transitioning initial -> healthy (or -> unhealthy when the security groups block the health check), and a GET through the load balancer returning the container's own body.
AWS: Smithy (elasticloadbalancingv2) CreateTargetGroup {TargetType ip, Port, HealthCheckPath}; CreateListener DefaultActions[] {Type "forward", TargetGroupArn}; DescribeTargetHealth -> TargetHealthDescriptions[].{Target{Id,Port}, TargetHealth{State, Reason, Description}}; and Smithy (ecs) CreateService loadBalancers[] {targetGroupArn, containerName, containerPort}. On AWS, registering an awsvpc ECS service into an ip target group causes ECS to register each task's ENI private IP as a target; the target reports State "initial" and then "healthy" with no Reason once the container answers the health-check path, and the listener then forwards requests to it so the load balancer returns the container's response verbatim (HTTP 200 and the body the container wrote). When the task's security group does not admit the load balancer's security group, the target instead settles at State "unhealthy" with a Reason in the documented set (Target.FailedHealthChecks / Target.Timeout) rather than hanging in "initial". Status/error codes: these are 200-level data-plane and control-plane calls; the divergence is non-convergence, not a refusal.
Emulator: recorded behavior only — the checks produce no AWS response to compare, because the terminal condition is never reached inside the check's budget: all four fail with `Check timed out after 60.0s`. Three of them are happy-path convergence waits (target registered; target healthy then the ALB serves the container's marker body; every request landing on the healthy target while an unreachable sibling is excluded) and the fourth is the negative control that needs the blocked target to settle at `unhealthy`.
Why: a client that stands up the standard ECS-behind-ALB topology gets a system that never finishes converging: no target health ever becomes authoritative, so nothing downstream of real traffic (routing decisions, ApplicationELB RequestCount/TargetResponseTime, alarms on them, health-based exclusion of a bad target) is reachable, and a wait-for-healthy loop — which is what every deploy script does — hangs instead of either succeeding or reporting an unhealthy Reason the operator can act on.
Fix: the ELBv2 target-health state machine (it must drive a registered target initial -> healthy when the target answers the health check, and initial -> unhealthy with a documented Reason when it cannot be reached) together with the ECS service-to-target-group registration that must publish the task's ENI IP as an ip target, and the listener/rule forwarding path that proxies the request to that target and returns its response.
Adjacent open filings that do NOT carry these four replay keys (they were filed from different, assertion-level signatures of this topology and the suite did not mark any of these four OWNED): 20261110T220525Z-cloudwatch-ecs-elbv2-logs-an-alb-answers-502-47cb37.md, 20261108T191814Z-cloudwatch-ecs-elbv2-logs-an-alb-answers-502.md, 20261105T204413Z-cloudwatch-ecs-elbv2-logs-an-unreachable-ip-target.md, 20261105T143800Z-cloudwatch-ecs-elbv2-logs-3-net-new-causes.md, 20261109T013920Z-cloudwatch-ecs-elbv2-logs-an-ecs-service-with.md, 20261114T200121Z-cloudwatch-ecs-elbv2-logs-a-target-the-alb.md and 20261104T062603Z-cloudwatch-ecs-elbv2-logs-task-attachments-expose-no.md.
Traces: cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks, .alb-health-check-reflects-ecs-task-health, .alb-routes-traffic-only-to-healthy-ecs-targets, .alb-security-group-mismatch-blocks-health-checks.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading cloudwatch__ecs__elbv2__logs 9/22 (41%), 12 failed — 8 marked OWNED, the four ids below survive and all four share one signature:

[mismatch] cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks
  Error: Check timed out after 60.0s
  Expected: DescribeTargetHealth lists the service task's ENI IP as a registered ip target. Actual: never observed inside the budget.

[mismatch] cloudwatch__ecs__elbv2__logs.alb-health-check-reflects-ecs-task-health
  Error: Check timed out after 60.0s
  Expected: TargetHealth.State "healthy" with Reason absent once the container answers "/", then HTTP 200 through the load balancer whose body is the marker the container wrote. Actual: never observed inside the budget.

[mismatch] cloudwatch__ecs__elbv2__logs.alb-routes-traffic-only-to-healthy-ecs-targets
  Error: Check timed out after 60.0s
  Expected: the permanently unreachable target reported unhealthy and excluded from routing while every request lands on the healthy task. Actual: never observed inside the budget.

[mismatch] cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks
  Error: Check timed out after 60.0s
  Expected: with the task SG not admitting the ALB SG, the target settles at State "unhealthy" with a documented Reason. Actual: never observed inside the budget.

The 60.0s figure is each check's own budget, not a requirement; the defect is that the state machine never settles at all. These four are the only surviving failures under this heading and they are all the same wait for the same topology.

Smithy consulted (models services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/… and services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/…): CreateTargetGroup/CreateListener/DescribeTargetHealth shapes above, with TargetHealth.Reason documenting Target.FailedHealthChecks / Target.Timeout / Target.ResponseCodeMismatch as the unhealthy reason codes; ECS CreateService loadBalancers[] {targetGroupArn, containerName, containerPort}. The convergence timing itself is recorded behavior only.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; all four ids in trace_ids reproduce with the same "Check timed out" signature.
2. Create a VPC-default ALB across two subnets with an SG open on 80, plus a task SG that admits the ALB SG on 80.
3. CreateTargetGroup (TargetType ip, Port 80, HealthCheckPath "/") and CreateListener on HTTP:80 forwarding to it.
4. RegisterTaskDefinition for an awsvpc Fargate nginx container that writes a known marker into its index page, and CreateService with desiredCount 1 and loadBalancers=[{targetGroupArn, containerName, containerPort 80}].
5. Poll DescribeTargetHealth. Expected: one target whose Id is the task's ENI private IP, moving from "initial" to "healthy" with no Reason. Actual: the condition is never reached.
6. GET http://<load balancer address>/. Expected: HTTP 200 with the container's marker body. Actual: never served.
7. Repeat step 4 with a task SG that does NOT admit the ALB SG. Expected: the target settles at "unhealthy" with Reason Target.FailedHealthChecks/Target.Timeout. Actual: it never leaves the non-terminal state.
8. Re-replay the four ids in trace_ids to confirm the fix.
