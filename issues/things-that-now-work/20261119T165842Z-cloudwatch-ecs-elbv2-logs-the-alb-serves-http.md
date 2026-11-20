Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks|run#19|AssertionError|assert status != 200, f"ALB served 200 for an unreachable target (status={status})"
Issue-Commit: 1a6df7b32e4c8350e561c708bcd111a16150a036

# cloudwatch__ecs__elbv2__logs: the ALB serves HTTP 200 from an ECS target whose security group blocks all ingress from the load balancer, so security-group reachability is never enforced in forwarding or in target health

> _Note: this issue was identified against the code base at commit `1a6df7b32e4c8350e561c708bcd111a16150a036`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause, reached by 1 check (Cause 1 of 1).

Cause 1 of 1: an Application Load Balancer forwards to — and serves a successful HTTP 200 from — an awsvpc ECS task whose security group has NO inbound rule admitting the load balancer's security group, and the registered target is never reported unhealthy for that unreachability.

AWS: Smithy (elasticloadbalancingv2, model services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/smithy-model/com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10.json): DescribeTargetHealth -> TargetHealthDescriptions[].TargetHealth {State: TargetHealthStateEnum, Reason: TargetHealthReasonEnum, Description}; the Reason member's own documentation enumerates, for State 'unhealthy', `Target.Timeout` ("The health check requests timed out") and `Target.FailedHealthChecks` ("The load balancer received an error while establishing a connection to the target or the target response was malformed"); CreateListener DefaultActions[] {Type 'forward', TargetGroupArn}. Health checks and forwarded requests both originate from the load balancer's own network interfaces, so a target security group with zero inbound rules cannot be reached on the traffic/health-check port. The requirement is therefore twofold and independent of any per-run value: (a) the registered target NEVER attains State 'healthy' while that security group blocks ingress — it leaves 'initial' (Reason Elb.InitialHealthChecking) and settles at 'unhealthy' with Reason Target.Timeout or Target.FailedHealthChecks; and (b) a request to the listener never returns the container's own response — the load balancer answers its own 5xx (502/504 when it cannot get a well-formed response from the target, 503 when no target is routable), because no TCP connection to the container can be established.

Emulator: the first request through the listener is answered HTTP 200 with the container's body, on the first poll iteration, while the target's security group admits nothing. Replay: AssertionError "ALB served 200 for an unreachable target (status=200)". The 200 is the defect; the check never even gets to grade the terminal 'unhealthy' state, because the 200 fails first. The emulator's ELBv2 data plane resolves the target group to the task's address and proxies to it with no evaluation of security-group reachability, and its target-health state machine does not derive an unhealthy state from an unreachable target.

Why: a client uses security groups as the enforcement boundary between its load balancer and its tasks. Against this emulator a task that is in fact unreachable is served as if healthy, so (i) a misconfigured security group is invisible — nothing turns unhealthy, nothing 5xxs, and the client ships a topology that fails the moment it meets real AWS; and (ii) the inverse test a security review relies on ("prove that removing the ingress rule stops traffic") passes when it must fail, which is a false negative on an access-control boundary rather than a cosmetic difference.

Fix: the ELBv2 data-plane forwarding path (the listener/rule action evaluation that resolves a target group to a registered address must only connect to an address the load balancer's security group is permitted to reach, and must generate the load balancer's own 502/504/503 otherwise) together with the ELBv2 target-health state machine, which must drive a security-group-blocked target through 'initial' to 'unhealthy' with Reason Target.Timeout / Target.FailedHealthChecks instead of leaving it servable.

Traces: cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks.

Related open filings that do NOT carry this check's replay key and do not cover it: 20261110T220525Z-cloudwatch-ecs-elbv2-logs-an-alb-answers-502-47cb37.md and 20261108T191814Z-cloudwatch-ecs-elbv2-logs-an-alb-answers-502.md describe the OPPOSITE polarity under this heading (a 502 for a target the control plane reports healthy); this cause is the ALB serving a 200 for a target it must not be able to reach at all, i.e. security groups are not consulted in either the health evaluation or the forwarding decision.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading cloudwatch__ecs__elbv2__logs, this failure is the only one under that heading the suite did not mark OWNED/ALREADY FILED.

[mismatch] cloudwatch__ecs__elbv2__logs.alb-security-group-mismatch-blocks-health-checks
  File "check_cloudwatch__ecs__elbv2__logs_alb_security_group_mismatch_blocks_health_checks.py", line 72, in run -> assert status != 200, f"ALB served 200 for an unreachable target (status={status})"
  AssertionError: ALB served 200 for an unreachable target (status=200)

Expected (AWS): with the task's security group carrying zero inbound rules, GET http://<load balancer address>/ returns a load-balancer-generated 5xx (502/504, or 503 with no routable target) or no response at all — never the container's 200 — and DescribeTargetHealth reports the ip target leaving 'initial' and settling at State 'unhealthy' with Reason Target.Timeout or Target.FailedHealthChecks.
Actual (emulator): HTTP 200 with the container's body on the first probe; the target is never reported unhealthy for unreachability.

Smithy consulted: elasticloadbalancingv2 DescribeTargetHealth output TargetHealthDescriptions[].TargetHealth {State, Reason, Description}; TargetHealthReasonEnum values include Elb.InitialHealthChecking, Target.Timeout, Target.FailedHealthChecks, Target.ResponseCodeMismatch; CreateListener DefaultActions[].{Type,TargetGroupArn}. The connectivity requirement itself is recorded behavior plus the ELBv2 target-group health-check documentation the check cites.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces.
2. In a VPC with two subnets, create security group A for the ALB and authorize inbound TCP 80 from the test client CIDRs.
3. Create security group B for the task and authorize NOTHING inbound (default allow-all egress only).
4. Create an internet-facing ALB in both subnets with security group A, an ip-type target group on port 80 with health-check path "/", and an HTTP:80 listener forwarding to it.
5. Run an awsvpc Fargate ECS service (nginx serving a known marker body) with desiredCount=1, security group B, registered into that target group, and wait for runningCount=1.
6. Poll DescribeTargetHealth and GET http://<load balancer address>/ in the same loop. Expected: the target never reaches 'healthy' and finally reports 'unhealthy' with Reason Target.Timeout / Target.FailedHealthChecks, and every GET returns a load-balancer 5xx or no response. Actual: the GET returns HTTP 200 with the container's marker body.
7. Re-replay the id in trace_ids to confirm the fix.
