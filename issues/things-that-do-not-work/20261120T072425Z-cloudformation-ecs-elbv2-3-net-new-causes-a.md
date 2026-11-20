Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecs__elbv2.cfn-alb-target-group-port-mismatch-negative cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive
Issue-Key: cloudformation__ecs__elbv2.cfn-alb-target-group-port-mismatch-negative|run#22|AssertionError|assert "healthy" not in states, (
Issue-Key: cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative|run#20|AssertionError|assert status2 == 504, (
Issue-Key: cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive|_poll_http_body#20|Timeout|time.sleep(interval_s)
Issue-Commit: 5f184b6d595470414f1315ced82dbc505d6a982b

# cloudformation__ecs__elbv2: 3 net-new causes — a port-mismatched target group reports healthy, an all-unhealthy forward falls back to the listener default 404 instead of 504, and a path-routing ECS+ALB stack never converges

> _Note: this issue was identified against the code base at commit `5f184b6d595470414f1315ced82dbc505d6a982b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
Three net-new root causes in the CloudFormation-provisioned ECS + ALB neighbourhood. Two of them are newly observable: the checks that previously only timed out now reach their assertions and diverge on concrete values.

Cause 1 of 3: an ELBv2 target group whose port is NOT the port the container listens on still reports its target as healthy.
AWS:      Smithy: com.amazonaws.elasticloadbalancingv2#TargetHealth.State -> TargetHealthStateEnum (initial | healthy | unhealthy | unhealthy.draining | unused | draining | unavailable); TargetHealth.Reason -> TargetHealthReasonEnum, documented as `Target.Timeout` ("The health check requests timed out") and `Target.FailedHealthChecks` ("The load balancer received an error while establishing a connection to the target or the target response was malformed"); TargetDescription.Port is "the port on which the target is listening". Recorded behavior: the health probe is sent to the registered target's port, so a target group on a port nothing listens on (the task's container publishes only its container port) transitions initial -> unhealthy with Reason `Target.Timeout`/`Target.FailedHealthChecks` and NEVER reaches `healthy`; the correctly wired sibling target group in the same stack does reach `healthy` and serves HTTP 200 (that positive control passes against the emulator).
Emulator: DescribeTargetHealth on the mismatched-port target group returns TargetHealthDescriptions whose TargetHealth.State set is {'healthy'} — the check fails on `target group with a container-port mismatch must never report healthy, got {'healthy'}`. The emulator marks a registered target healthy without probing the target's port at all, so a port that cannot be connected to is indistinguishable from a working one.
Why:      every infrastructure test whose subject is "a misconfigured container/target-group port is caught" passes against the emulator while the same stack is broken on AWS. Health is the signal clients gate deployments and rollbacks on; a health state that is unconditionally `healthy` turns the ALB's main failure detector off and hides the most common ECS wiring mistake.
Fix:      the ELBv2 target-health evaluation (the health-probe/target-health component of the elasticloadbalancingv2 service and the state it derives TargetHealth from) must base State/Reason on an actual connection to the registered target's port (TargetGroup.Port, or TargetDescription.Port when overridden, against the task's published container port) instead of reporting `healthy` on registration.
Traces:   cloudformation__ecs__elbv2.cfn-alb-target-group-port-mismatch-negative.

Cause 2 of 3: with every target in the matched target group unhealthy, the ALB answers the listener's default action (404) instead of forwarding and timing out (504).
AWS:      recorded behavior only — this is a data-plane behaviour, not a modelled error. The listener has a default action of type `fixed-response` with StatusCode 404 and MessageBody "no-rule-matched", plus a rule at priority 1 whose condition is `path-pattern` ['/blocked/*'] forwarding to the blocked target group. A request whose path matches a rule is handled by THAT rule: the ALB fails open when all targets of the forward group are unhealthy and forwards to them anyway; the task security group (no ingress from the ALB's security group) drops the connection, and after the ALB's target connect timeout the client receives HTTP 504 (gateway timeout). It is never 503 and never the listener's default action. In the same check the preceding assertions pass: the blocked target reaches 'unhealthy' and never 'healthy', and the open-SG sibling serves HTTP 200.
Emulator: `GET http://<alb-dns-name>/blocked/` returns HTTP 404 — the check fails on `expected 504 (the ALB's forward to the SG-blocked target times out) for the SG-blocked path, got 404`. 404 is exactly the listener's default fixed-response status, so the matched `path-pattern` rule is not taking effect for this request: either the rule's path-pattern match does not fire for '/blocked/' against '/blocked/*', or a forward action whose target group has no healthy target is abandoned and evaluation falls through to the listener's default action. Hypothesis: the forward is skipped when no target is healthy rather than failing open.
Why:      a client cannot distinguish "the network boundary blocked the target" (504) from "no rule matched my path" (404). The two have opposite diagnoses: the first points at a security group, the second at listener-rule configuration. A test that asserts an ALB still forwards to an unhealthy-but-registered target set (fail-open) observes the wrong status, and any health-gated 503/504 handling built on it is never exercised.
Fix:      the ALB data-plane request router of the elasticloadbalancingv2 service — rule evaluation must route a path-matching request to its rule's forward action regardless of target health (fail open when no target is healthy) and surface the resulting connect timeout as HTTP 504, instead of falling back to the listener's default action.
Traces:   cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative.

Cause 3 of 3: a CloudFormation/ECS + ALB path-based-routing stack never converges, so the check reaches no assertion.
AWS:      recorded behavior only. The check creates a VPC/subnets, an execution role, an ECS cluster, two task definitions, an ALB with a 404 fixed-response default action and two `path-pattern` rules ('/svc-a*' and '/svc-b*') forwarding to two target groups, then two Fargate services. On AWS each control-plane call returns promptly and the services and target groups settle, so the path-routing assertions (each path reaches only its own service, the unmatched path gets the default 404) are reached.
Emulator: the check ends with `Error: Check timed out after 60.0s` — no assertion is reached; the ECS services / target registrations behind the ALB never settle inside the budget, while sibling checks in this same service do converge, so this is not a global budget shortfall.
Why:      the single most common containerised web shape (one ALB, several ECS services split by path) cannot be observed working at all: a client's deploy-then-verify pipeline hangs rather than passing or failing with a diagnosis.
Fix:      the ECS service / ELBv2 target-registration convergence path (AWS::ECS::Service + RegisterTargets/target-health settling) so a created service and its targets reach a steady state promptly.
Related open issue that looks similar but does NOT carry this check's replay key (the replay report lists it as unowned): 20261114T080626Z-cloudformation-ecs-elbv2-five-cloudformation-ecs-alb-scenarios.md. For causes 1 and 2, the previously filed timeout issues 20261114T080626Z-cloudformation-ecs-elbv2-five-cloudformation-ecs-alb-scenarios.md and 20261117T210102Z-cloudformation-ecs-elbv2-the-security-group-blocked-alb.md described those same checks as reaching NO verdict; they now reach a verdict and diverge on the values above, which neither issue describes.
Traces:   cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive.

## Evidence
Cause 1 — #### [mismatch] cloudformation__ecs__elbv2.cfn-alb-target-group-port-mismatch-negative
  AssertionError: target group with a container-port mismatch must never report healthy, got {'healthy'}
  Expected (AWS): DescribeTargetHealth on the target group whose port is not the container's listening port reports TargetHealth.State 'unhealthy' (Reason Target.Timeout / Target.FailedHealthChecks) and never 'healthy'. Fixed text of the requirement is the enum value itself; the port numbers (8080 vs 80) come from the request/template, not from any one recording.
  Actual (emulator): TargetHealth.State == 'healthy' for every description in that group.

Cause 2 — #### [mismatch] cloudformation__ecs__elbv2.cfn-security-group-blocks-alb-to-ecs-traffic-negative
  AssertionError: expected 504 (the ALB's forward to the SG-blocked target times out) for the SG-blocked path, got 404
  Expected (AWS): HTTP 504 from `GET http://<alb-dns-name>/blocked/` after the target connect timeout (the path matches the priority-1 path-pattern rule '/blocked/*' forwarding to the blocked target group; all its targets are 'unhealthy', the ALB fails open, the task SG drops the connection).
  Actual (emulator): HTTP 404 — the listener's default fixed-response (StatusCode '404', MessageBody 'no-rule-matched'); the 404 status and that body are the listener's configured default action, i.e. the matched rule did not route the request. The earlier assertions in the same check pass: the blocked target is 'unhealthy' (never 'healthy') and the open-SG sibling returns HTTP 200 with the nginx page.

Cause 3 — #### [mismatch] cloudformation__ecs__elbv2.elbv2-listener-rule-path-based-routing-to-multiple-ecs-services-positive
  Error: Check timed out after 60.0s
  Expected (AWS): the two Fargate services settle and their targets register, so '/svc-a/' reaches service A, '/svc-b/' reaches service B (body 'sim2real-hello-from-b') and an unmatched path gets the listener's default 404.
  Actual (emulator): no assertion reached; the run is cut off at the harness cap with no status code, health state or body produced.

## Steps to Reproduce
Cause 1:
1. CreateStack with a VPC, two subnets, an internet-facing ALB + HTTP:80 listener, two target groups (one on the container's port, one on a port nothing listens on) and two Fargate ECS services wired to them.
2. Scale the mismatched-port service to desiredCount=1 and wait for steady state.
3. Call elbv2 DescribeTargetHealth on the mismatched-port target group and read TargetHealth.State.
4. Observe 'healthy' where AWS reports 'unhealthy' with Reason Target.Timeout/Target.FailedHealthChecks.

Cause 2:
1. In the same shape, give the blocked service's task security group NO ingress from the ALB's security group, and add a listener rule at priority 1 with condition path-pattern ['/blocked/*'] forwarding to that service's target group; keep the listener default action a fixed-response 404 'no-rule-matched'.
2. Wait until DescribeTargetHealth on the blocked group reports 'unhealthy'.
3. GET http://<alb-dns-name>/blocked/ with a client timeout above the ALB's target connect timeout.
4. Observe HTTP 404 (the listener default) instead of HTTP 504.

Cause 3:
1. Create an ECS cluster, two task definitions, an ALB with a fixed-response-404 default action and two path-pattern rules ('/svc-a*', '/svc-b*') forwarding to two target groups, then two Fargate services wired to those groups.
2. Poll ecs DescribeServices for steady state and elbv2 DescribeTargetHealth for registered targets.
3. Observe the run never settles: the check is cut off at its 60 s budget before any routing assertion.
