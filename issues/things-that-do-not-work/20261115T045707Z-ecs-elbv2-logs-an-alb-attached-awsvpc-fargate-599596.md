Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.ecs-service-behind-alb-serves-http-request ecs__elbv2__logs.ecs-update-service-to-a-different-network-mode-rejected
Issue-Key: ecs__elbv2__logs.ecs-service-behind-alb-serves-http-request|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: ecs__elbv2__logs.ecs-update-service-to-a-different-network-mode-rejected|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Commit: da3e7884167b3f933d6f1406ab01004ac62fd391

# ecs__elbv2__logs: an ALB-attached awsvpc Fargate service never serves an HTTP response, so both the happy path and the awsvpc→bridge UpdateService refusal are cut off before any assertion

> _Note: this issue was identified against the code base at commit `da3e7884167b3f933d6f1406ab01004ac62fd391`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
One net-new root cause with two grounded checks (Cause 1 of 1).

Cause 1 of 1: an awsvpc Fargate ECS service attached to an ALB target group never reaches the state where the load balancer serves the container's response, so both surviving checks under this heading exhaust their budget ("Check timed out after 60.0s") before they can assert anything — including the negative control, whose refusal is only reachable once the service is live and answering.
AWS: Smithy (ecs) CreateService loadBalancers[] {targetGroupArn, containerName, containerPort} with networkConfiguration.awsvpcConfiguration {subnets, securityGroups}; UpdateService errors include InvalidParameterException (client, HTTP 400). Smithy (elasticloadbalancingv2) CreateListener DefaultActions[] {Type "forward", TargetGroupArn} and DescribeTargetHealth -> TargetHealthDescriptions[].TargetHealth{State, Reason}. On AWS the service's task gets an ENI with a private IP, that IP is registered as an ip target, the target turns healthy, and a GET through the load balancer returns the container's own response; and UpdateService refuses to move a live awsvpc, ALB-attached service onto a bridge-mode task-definition revision with InvalidParameterException, after which the service keeps answering HTTP on the original revision. Status/error codes: the happy path is 200-level; the refusal is InvalidParameterException at HTTP 400.
Emulator: recorded behavior only — neither check obtains a comparable response. Both are cut off by the harness while still waiting for the service to answer through the load balancer; the network-mode refusal is never exercised at all, so the emulator's UpdateService behaviour on that transition is unverified rather than wrong-and-recorded.
Why: the single most common ECS topology (Fargate service behind an ALB) is unusable — nothing a client builds on served traffic can be reached, and because the negative control cannot run, an emulator that WOULD wrongly accept an awsvpc→bridge UpdateService is indistinguishable from one that refuses it correctly. A client's deploy wait loop hangs rather than failing with a diagnosable reason.
Fix: the ECS awsvpc task networking that must give the task an ENI private IP, the ECS service -> target-group registration that publishes that IP as an ip target, and the ELBv2 listener forwarding path that proxies a request to it; once the service serves, UpdateService must also refuse a network-mode change on a live load-balanced service with InvalidParameterException.
Adjacent open filings that do NOT carry these two replay keys (filed from earlier, different signatures of the same topology; the suite did not mark either check OWNED): 20261108T200250Z-ecs-elbv2-logs-an-alb-fronted-awsvpc-fargate.md, 20261107T222655Z-ecs-elbv2-logs-an-alb-attached-awsvpc-fargate.md, 20261105T143800Z-ecs-elbv2-logs-2-net-new-causes-an.md and 20261104T062603Z-ecs-elbv2-logs-an-awsvpc-task-never-gets.md.
Traces: ecs__elbv2__logs.ecs-service-behind-alb-serves-http-request, ecs__elbv2__logs.ecs-update-service-to-a-different-network-mode-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading ecs__elbv2__logs 0/6 (0%), 4 failed (2 marked OWNED, 2 unloadable); the two ids below survive with one shared signature:

[mismatch] ecs__elbv2__logs.ecs-service-behind-alb-serves-http-request
  Error: Check timed out after 60.0s
  Expected: the ALB-attached awsvpc Fargate service's target turns healthy and a GET through the load balancer returns the container's response. Actual: never reached inside the budget.

[mismatch] ecs__elbv2__logs.ecs-update-service-to-a-different-network-mode-rejected
  Error: Check timed out after 60.0s
  Expected: the live service answers HTTP, UpdateService onto a bridge-mode revision is refused with InvalidParameterException, and the service keeps answering on the original revision. Actual: the check never gets past bringing the service up, so the refusal is never exercised.

The 60.0s is each check's own budget, not a requirement; the defect is that the topology never converges.

Smithy consulted (models services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/… and services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/…): ECS CreateService loadBalancers[]/networkConfiguration.awsvpcConfiguration and UpdateService errors {AccessDenied, Client, ClusterNotFound, InvalidParameter, Platform*, Server, ServiceNotActive, ServiceNotFound, …}; ELBv2 CreateListener DefaultActions[] and DescribeTargetHealth TargetHealth{State,Reason}. The convergence timing is recorded behavior only.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; both ids in trace_ids reproduce with the same "Check timed out" signature.
2. Create an ALB across two subnets with an SG open on 80, an ip-type target group on port 80, and an HTTP:80 listener forwarding to it.
3. RegisterTaskDefinition (awsvpc, Fargate, an HTTP container on port 80 writing a known body) and CreateService with desiredCount 1, awsvpcConfiguration subnets/securityGroups, and loadBalancers=[{targetGroupArn, containerName, containerPort 80}].
4. Poll DescribeTargetHealth for State "healthy", then GET http://<load balancer address>/. Expected: HTTP 200 with the container's body. Actual: neither is ever observed.
5. Register a second revision of the same task definition with networkMode "bridge" and call UpdateService to move the live service onto it. Expected: InvalidParameterException and the service still answering HTTP on the awsvpc revision. Actual: unreachable, because step 4 never completes.
6. Re-replay both ids in trace_ids to confirm the fix.
