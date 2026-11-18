Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config
Issue-Key: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config|run#14|Timeout|time.sleep(2)
Issue-Commit: 505e225be56e8a8dc67d417f965dd4a20cae2870

# ecr__ecs__elbv2: the no-loadBalancers negative control never reaches a verdict — a Fargate service's running task is never registered as an ELBv2 ip target, so the positive control never settles

> _Note: this issue was identified against the code base at commit `505e225be56e8a8dc67d417f965dd4a20cae2870`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause (Cause 1 of 1), grounded in the single surviving failing check under the `ecr__ecs__elbv2` heading. The replay report marks it neither ALREADY FILED nor OWNED, so it survives as a live failure.

Cause 1 of 1: an ECS Fargate service created with a `loadBalancers` binding (awsvpc networking, ip-type target group) never has its running task's ENI private address registered into that target group, so `elbv2 DescribeTargetHealth` on the wired group never grows to one TargetHealthDescription. The check's positive control therefore never satisfies and the scenario is cut off at the harness ceiling before the negative assertion (an unreferenced target group must stay empty) is ever graded.

AWS: recorded behavior only for the registration side effect — the ECS model describes the binding, not the act of registering. Smithy: com.amazonaws.ecs#LoadBalancer.{targetGroupArn,containerName,containerPort} is the service's declared binding (and its doc states that an awsvpc-mode task definition requires target type `ip`, because the task is associated with an elastic network interface rather than an EC2 instance); com.amazonaws.elasticloadbalancingv2#DescribeTargetHealth takes a required TargetGroupArn and returns TargetHealthDescriptions[].{Target{Id,Port},TargetHealth{State,Reason,Description}} (errors TargetGroupNotFound/InvalidTarget/HealthUnavailable, none applicable here — every call in this scenario succeeds with HTTP 200). On AWS, as each task of a service declaring `loadBalancers` reaches RUNNING, its ENI private IPv4 address is registered into the named target group: DescribeTargetHealth then lists exactly one TargetHealthDescription per running task, Target.Id equal to that private IP and Target.Port equal to the declared containerPort, and the load balancer probes it until TargetHealth.State settles (healthy for a target that answers the configured health check). A target group that no service references keeps an empty TargetHealthDescriptions list in every state — never an entry marked unhealthy — and that contrast is exactly what makes the binding observable. Values such as the IP, the group ARN and the generated names are per-run; the requirement is the one-entry-per-running-task registration driven solely by the `loadBalancers` binding.

Emulator: the check produces only `Error: Check timed out after 60.0s`. No status code, error code or body diverges — every control-plane call returns 200 — but the graded signals never settle: DescribeServices runningCount never reaches 1 for either service and DescribeTargetHealth on the ALB-wired group never returns a single TargetHealthDescription, so `_wait_running_count(..., 1, timeout_s=360)` / `_wait_target_count(tg_wired, 1, timeout_s=180)` keep polling until the harness kills the run. The 60 s sustained-emptiness window on the unreferenced target group is never entered, so the emulator never shows whether registration follows the binding at all.

Why: a client that deploys a Fargate service behind an ALB gets a load balancer with no targets (503 on the data path) and nothing in the ECS or ELBv2 responses to explain it; a deployment gate that waits for a target to appear or become healthy hangs instead of returning a verdict. Worse for fidelity, because the positive control is broken the negative case is indistinguishable: an emulator that registers every running task regardless of the binding, and one that registers nothing, read identically, so mis-wired `loadBalancers` configurations cannot be diagnosed.

Fix: the ECS service/task placement path that must register an awsvpc task's private address and declared containerPort into each configured target group as the task reaches RUNNING (and deregister on stop), the task-state progression behind DescribeServices runningCount / DescribeTasks attachments, and the ELBv2 target-registration state read by DescribeTargetHealth, which must list one entry per registered running task and publish a terminal TargetHealth.State — and must keep a group no service references empty.

Related filings that the replay report does NOT mark as owning this check id (they must not be assumed to cover it): open 20261114T193643Z-ecr-ecs-elbv2-an-ecs-service-s-running.md (lists this id but carries a stale replay key `_poll_until#9`, while the failure now surfaces as a plain harness timeout), open 20261111T095650Z-ecr-ecs-elbv2-ecr-image-backed-ecs-services.md, open 20261112T060648Z-ecr-ecs-elbv2-an-ecr-image-backed-fargate.md, open 20261110T220525Z-ecr-ecs-elbv2-the-ecs-container-health-alb.md, and the already-resolved 20261110T143849Z-ecr-ecs-elbv2-an-ecs-service-created-with.md / 20261105T141707Z-ecr-ecs-elbv2-a-fargate-service-with-desiredcount-fe3e95.md, whose closure did not make this check converge (it is not a refusal: nothing is refused, the scenario simply never settles).

Traces: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator. Heading `ecr__ecs__elbv2` 8/17 (47%), 7 failed (2 unloadable); six of the failures are marked OWNED, this one is not:

  #### [mismatch] ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config
  Error: Check timed out after 60.0s

Check body (abridged): create a VPC with two subnets and a security group; create an ECS cluster; RegisterTaskDefinition (FARGATE, networkMode awsvpc, container `app` on containerPort 80 serving / over HTTP); create one ip-type target group that nothing references (`tg_unused`) and an ALB + listener + ip-type target group (`tg_wired`, HealthCheckProtocol HTTP, path /, interval 10, healthy threshold 2, matcher 200); CreateService twice with desiredCount=1 — once with no `loadBalancers`, once with loadBalancers=[{targetGroupArn: tg_wired, containerName: app, containerPort: 80}] and healthCheckGracePeriodSeconds=120. Then `_wait_running_count(..., 1, timeout_s=360)` for both services, `_wait_target_count(tg_wired, 1, timeout_s=180)` as the positive control, and finally a 60 s loop asserting describe_target_health(tg_unused).TargetHealthDescriptions == [].

Expected (AWS, HTTP 200 throughout): both services report runningCount 1; DescribeTargetHealth(tg_wired) lists exactly one TargetHealthDescription whose Target.Id is the wired service's task ENI private IPv4 address and Target.Port is 80, settling to TargetHealth.State 'healthy'; DescribeTargetHealth(tg_unused) returns TargetHealthDescriptions == [] continuously, and DescribeServices for the unwired service reports loadBalancers as an empty list.

Actual (emulator): no graded value at all — the run is killed at the 60 s harness cap with `Error: Check timed out after 60.0s`. runningCount never reaches 1 and DescribeTargetHealth(tg_wired) never returns a single entry, so the positive control never satisfies and the sustained-empty assertion on tg_unused is never reached. No error code or non-2xx status is involved; the divergence is the missing registration/convergence effect.

Smithy consulted: elasticloadbalancingv2 DescribeTargetHealth (required TargetGroupArn; output TargetHealthDescriptions[].{Target{Id,Port},TargetHealth{State,Reason,Description}}; errors TargetGroupNotFound, InvalidTarget, HealthUnavailable); ecs LoadBalancer.{targetGroupArn,containerName,containerPort} and its awsvpc ⇒ target type `ip` requirement.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces the failure.
2. Create a VPC (10.99.0.0/16) with two public subnets, an internet gateway/route table and a self-referencing security group; create an ECS cluster.
3. RegisterTaskDefinition with requiresCompatibilities [FARGATE], networkMode awsvpc, cpu 256 / memory 512 and one essential container `app` with portMappings containerPort 80 serving an HTTP 200 on /.
4. CreateTargetGroup twice in that VPC with TargetType ip, Protocol HTTP, Port 80: one left unreferenced, one attached to a new internet-facing ALB through an HTTP:80 listener whose default action forwards to it.
5. CreateService with desiredCount=1, launchType FARGATE and awsvpcConfiguration over both subnets — once WITHOUT `loadBalancers`, once WITH loadBalancers=[{targetGroupArn: <wired group>, containerName: app, containerPort: 80}].
6. Poll DescribeServices until runningCount==1 for both services, then poll elbv2 DescribeTargetHealth(TargetGroupArn=<wired group>) for exactly one target. Expected: one entry whose Target.Id is the task's ENI private IP and Target.Port is 80, reaching State 'healthy'. Actual: runningCount and the target list never settle; the check is cut off at 60 s and the negative control (DescribeTargetHealth on the unreferenced group staying []) is never graded.
7. Re-replay ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config to confirm the fix.
