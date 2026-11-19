Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503
Issue-Key: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503|_poll_until#27|Timeout|last = probe()
Issue-Commit: 6e2d7185f30747a2356e601c1d412ac8e0be11c4

# ecs__elbv2__logs: the ALB fail-open (single unhealthy target still served 200) and the empty-target-group 503 are never observable — the check is cut off at its budget with no request ever answered

> _Note: this issue was identified against the code base at commit `6e2d7185f30747a2356e601c1d412ac8e0be11c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateListener

## Description
1 root cause.

=== Cause 1 of 1: an ALB fronting an ECS service never answers on its published endpoint, so neither fail-open nor the empty-group 503 can be reached ===
Failing check: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503
AWS: when every target in an Application Load Balancer's target group is unhealthy the load balancer fails open and routes to the unhealthy targets anyway, so the client still receives the target's 200 even while DescribeTargetHealth reports State='unhealthy' with Reason='Target.ResponseCodeMismatch'; when the target group holds no targets at all the load balancer itself generates HTTP 503 Service Unavailable; and restoring the health-check path returns the target to State='healthy' with the same 200s. Reference the check cites: https://docs.aws.amazon.com/elasticloadbalancing/latest/application/target-group-health-checks.html. Smithy (elasticloadbalancingv2): CreateTargetGroup/ModifyTargetGroup carry HealthCheckPath, RegisterTargets/DeregisterTargets change membership, DescribeTargetHealth reports TargetHealth.State and .Reason — but the three behaviours above exist only on the data path, so they are only observable by sending a request to the load balancer.
Emulator: the check never completes a single observable step — it is killed with "Check timed out after 60.0s". The ALB in front of the ECS service never answers at its published endpoint, so neither the fail-open 200, nor the generated 503 of an emptied group, nor the restored healthy 200 is ever produced.
Why: a client gets no answer where AWS serves one, and the two safety behaviours customers rely on (fail-open instead of a blackhole, and a distinguishable load-balancer-generated 503 for an empty group) cannot be depended on at all. The distinction matters: a 503 with no target vs a 200 from an unhealthy target is how a client tells "nothing registered" from "registered but failing".
Fix: the ELBv2 data plane behind the listener — the forward action's target selection when no target is healthy (fail open) and the load-balancer-generated 503 for an empty target group — plus whatever keeps the ECS-backed target group from converging to a servable state inside the budget. Related open issues describe this family for other check ids but do not carry this one: 20261111T010150Z-ecs-elbv2-logs-alb-fail-open-with-a-single-unhealthy-ecs.md and 20261110T220525Z-ecs-elbv2-logs-2-net-new-causes-the.md (the latter owns the sibling container-health check in this suite, not this one).
Traces: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503.

## Evidence
Expected: (1) with the only target unhealthy (DescribeTargetHealth State='unhealthy', Reason='Target.ResponseCodeMismatch') a GET to the load balancer endpoint still returns HTTP 200 with the target's body; (2) after deregistering every target, a GET returns a load-balancer-generated HTTP 503; (3) after restoring the health-check path, State returns to 'healthy' and GETs return 200.
Actual: "Error: Check timed out after 60.0s" — the ALB never answers at its published endpoint, so none of the three observations is ever made and the check reaches no verdict.

## Steps to Reproduce
Create a VPC-default ALB with two subnets and a security group open on 80, an ECS service whose task serves HTTP on 80 registered as the only ip target of a target group, and an HTTP listener forwarding to it. Point the target group's HealthCheckPath at a path the container answers with a non-matching status so DescribeTargetHealth reports State='unhealthy' / Reason='Target.ResponseCodeMismatch', then GET the load balancer's published endpoint (expect 200, fail-open). DeregisterTargets so the group is empty and GET again (expect a load-balancer-generated 503). Restore HealthCheckPath and GET again (expect 'healthy' and 200). The emulator answers none of the three; the check is killed at its 60 s budget.
