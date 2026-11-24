Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503
Issue-Key: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Commit: b35089c281a7e84768d928c8c72034d27eee1ae8

# ecs__elbv2__logs: the ALB fail-open / empty-group-503 behaviour never becomes observable

> _Note: this issue was identified against the code base at commit `b35089c281a7e84768d928c8c72034d27eee1ae8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _SPLIT: this report was filed with two causes. Cause 2 — a container's `healthStatus` never reaching a terminal value, so no task replacement is driven — is NOT fixed, and is re-filed with its own trace id and Issue-Key as `issues/things-that-do-not-work/20261124T120000Z-ecs-elbv2-logs-a-container-s-cgi-endpoint-is-never-executed.md` so it stays filed and replayable. Only cause 1 below is answered here._

## Operation
DescribeTargetHealth

## Description
Cause 1 of 2: the ALB's fail-open behaviour and its empty-target-group 503 are unobservable — the scenario never reaches a graded step and the check is killed by its ceiling.
AWS: Smithy (elasticloadbalancingv2) DescribeTargetHealth -> TargetHealthDescriptions[].TargetHealth {State, Reason, Description} (Reason documents Target.ResponseCodeMismatch for a health check that did not return an expected HTTP code); CreateListener DefaultActions[] {Type 'forward', TargetGroupArn}. AWS behaviour, per the target-group health-checks documentation the check cites: when EVERY target in the group is unhealthy the load balancer fails open and routes to them anyway, so the request is still served (HTTP 200 from the container); when the group has NO registered targets the load balancer answers its own HTTP 503; and repairing the health-check path returns the target to 'healthy' and the responses to healthy 200s. Those three outcomes are the requirement.
Emulator: "Check timed out after 60.0s" — no terminal target state and no served response is produced, so neither the fail-open 200, nor the empty-group 503, nor the recovery is reachable.
Why: fail-open is the behaviour that keeps a service reachable during a bad health-check rollout, and the empty-group 503 is how a client distinguishes "nothing registered" from "target refused". With neither observable, a client cannot predict what its load balancer does when health checks go wrong — the exact situation the behaviour exists for.
Fix: the ELBv2 target-health state machine (it must reach 'unhealthy' with Reason Target.ResponseCodeMismatch from a real probe) and the data-plane forwarding path (fail open when all targets are unhealthy; generate a 503 for an empty target group).
Traces: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading ecs__elbv2__logs 0/6 (0%), 4 failed (plus 2 unloadable).

Cause 1: [mismatch] ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503
  Error: Check timed out after 60.0s
  Expected (AWS): the only target reaches 'unhealthy' with Reason Target.ResponseCodeMismatch and the load balancer fails open, still serving HTTP 200; with no registered targets the load balancer answers HTTP 503; fixing the health-check path restores 'healthy' 200s.
  Actual (emulator): no terminal state and no served response within the budget.

Smithy consulted: elasticloadbalancingv2 DescribeTargetHealth output TargetHealthDescriptions[].TargetHealth {State, Reason, Description} and CreateListener DefaultActions[] (model services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10).

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces.
2. Create an ALB with an HTTP:80 listener forwarding to an ip target group whose health-check path returns a non-matching code; register the single ECS task ip. Expect the target 'unhealthy' with Reason Target.ResponseCodeMismatch and the load balancer to fail open with HTTP 200. Then deregister all targets and expect the load balancer's own HTTP 503. Then repair the health-check path and expect 'healthy' 200s.
3. Re-replay the id in trace_ids to confirm the fix.

## How it was answered
A target group created with NO health-check members — the documented defaults `CreateTargetGroup`
settles (`HealthCheckIntervalSeconds` 30, `HealthyThresholdCount` 5, `UnhealthyThresholdCount` 2) —
was checked by this emulator's own clock 30 real seconds apart, so `unhealthy` with
`Target.ResponseCodeMismatch` arrived 30 s after the checked path started answering outside the
matcher and the return to `healthy` took two and a half minutes: none of the three outcomes was
reachable in one run. The cadence between two checks is now the duration the CALLER asked for where
a request named `HealthCheckIntervalSeconds`, and the span this emulator settles its own transitions
in where no request named one; the thresholds are counts and stand as AWS reports them, and
`DescribeTargetGroups` still answers the documented defaults. The whole chain —
`unhealthy`/`Target.ResponseCodeMismatch` from a real probe, the fail-open 200 carrying the
container's own document (in the plane and over a real connection to the published address), the
repair back to `healthy`, and the load balancer's own 503 once its only registration is draining —
is driven end to end in `core/router/tests/all/alb_health_check_cadence_the_request_asked_for.rs`
over a table of the cadences a request can ask for.
