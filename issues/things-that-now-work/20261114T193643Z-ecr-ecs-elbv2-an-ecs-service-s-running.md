Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__elbv2.elbv2-target-group-registers-ecs-service-tasks ecr__ecs__elbv2.elbv2-health-check-reflects-ecs-task-status ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config
Issue-Key: ecr__ecs__elbv2.elbv2-target-group-registers-ecs-service-tasks|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Key: ecr__ecs__elbv2.elbv2-health-check-reflects-ecs-task-status|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Key: ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config|_poll_until#9|Timeout|time.sleep(interval_s)
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# ecr__ecs__elbv2: an ECS service's running task is never registered or health-probed as an ELBv2 target, so all three target-registration/health checks are cut off at their budget

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTargetHealth

## Description
One net-new root cause (Cause 1 of 1), grounded in three failing checks under the `ecr__ecs__elbv2` heading. None of the three is marked OWNED or ALREADY FILED by the replay report, so they survive as live failures.

Cause 1 of 1: an ECS service that declares a `loadBalancers` binding never gets its running task's awsvpc address registered into the named target group, and no registered target ever acquires a terminal health state, so every scenario that grades ECS/ELBv2 convergence — positive registration, container-health vs. target-health convergence, and the negative control that needs the positive case to register — hangs until the harness budget ends with no verdict.
AWS: recorded behavior only for the registration side effect (the ECS Smithy model describes the `loadBalancers` member, not the registration); the read surface is modeled: elasticloadbalancingv2 DescribeTargetHealth -> TargetHealthDescriptions[].{Target{Id,Port},TargetHealth{State,Reason,Description}} and ecs DescribeTasks -> tasks[].containers[].healthStatus plus tasks[].attachments[].details carrying the awsvpc ENI privateIPv4Address. On AWS, a service created with loadBalancers=[{targetGroupArn, containerName, containerPort}] and awsvpc networking has each task's ENI private IP registered into that target group as the task reaches RUNNING: DescribeTargetHealth then lists exactly one TargetHealthDescription per running task, with Target.Id equal to that private IP and Target.Port equal to the container port, and the load balancer independently probes it until State becomes 'healthy'. A target group that no service references stays an empty TargetHealthDescriptions list, and that contrast is what makes the wiring observable.
Emulator: all three checks are terminated with "Check timed out after 60.0s". The waits that never settle are DescribeServices runningCount==1, DescribeTasks containers[].healthStatus=='HEALTHY', the task attachment's privateIPv4Address, DescribeTargetHealth reaching one target, and that target reaching State 'healthy'. No terminal value is ever produced for any of the graded signals — the wired target group keeps reading back as if nothing were attached.
Why: a client that deploys an ECS service behind an ALB sees a load balancer with no targets (503s on the data path) and nothing in the ECS or ELBv2 responses to explain it; a deployment gate that waits for a target to become healthy, or for a container to report HEALTHY, hangs forever instead of returning healthy/unhealthy or an error; and the negative case ("a service without loadBalancers must register nothing") cannot be distinguished from the broken positive case, so wiring mistakes are undiagnosable.
Fix: the ECS service/task placement path that must register an awsvpc task's address and container port into each configured target group (and deregister it when the task stops), the container-health evaluation behind DescribeTasks healthStatus, and the ELBv2 target-health state machine DescribeTargetHealth reads, which must publish a terminal State/Reason for a registered target.
Related open filings that the suite did NOT mark as owning these checks (they describe the same registration/convergence gap and must not be assumed to cover these ids): 20261110T143849Z-ecr-ecs-elbv2-an-ecs-service-created-with.md, 20261110T220525Z-ecr-ecs-elbv2-the-ecs-container-health-alb.md, 20261109T141609Z-ecr-ecs-elbv2-a-container-with-a-healthcheck.md, 20261112T060648Z-ecr-ecs-elbv2-an-ecr-image-backed-fargate.md.
Traces: ecr__ecs__elbv2.elbv2-target-group-registers-ecs-service-tasks, ecr__ecs__elbv2.elbv2-health-check-reflects-ecs-task-status, ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; heading ecr__ecs__elbv2 5/17 (29%), 10 failed (2 unloadable). Seven failures are OWNED; these three are not.

[mismatch] ecr__ecs__elbv2.elbv2-target-group-registers-ecs-service-tasks
  Error: Check timed out after 60.0s
  Body: create VPC/subnets/SG, cluster, awsvpc task definition, ALB + ip target group, then CreateService with loadBalancers=[{targetGroupArn, containerName, containerPort=80}], desiredCount=1; _wait_running_count(...,1,timeout_s=360); _task_private_ips(...) must yield exactly one IP; _wait_target_count(tg,1,timeout_s=180) then asserts targets[0].Target.Id == that IP and targets[0].Target.Port == 80.
  Expected (AWS): one TargetHealthDescription whose Target.Id is the task ENI private IP and Target.Port is 80.
  Actual: the target count never reaches 1; the check is cut off with no verdict.

[mismatch] ecr__ecs__elbv2.elbv2-health-check-reflects-ecs-task-status
  Error: Check timed out after 60.0s
  Body: same setup, then _wait_container_health(...,"HEALTHY",timeout_s=300) asserting ['HEALTHY'], and _wait_target_state(tg, ip, "healthy", timeout_s=300) asserting 'healthy'.
  Expected (AWS): DescribeTasks reports containers[].healthStatus HEALTHY and DescribeTargetHealth reports that address State 'healthy' — the two independent signals converge.
  Actual: neither signal settles; cut off at the 60 s ceiling.

[mismatch] ecr__ecs__elbv2.elbv2-no-target-registration-without-service-lb-config
  Error: Check timed out after 60.0s
  Body: an unreferenced target group plus an ALB-wired target group; two identical services, one with tg_arn=None and one wired; both waited to runningCount=1, then the positive control waits for the wired group to hold exactly one target before asserting the unreferenced group stays [].
  Expected (AWS): wired group lists exactly one target; unreferenced group stays [].
  Actual: the positive control never satisfies, so the negative assertion is never graded.

Smithy consulted: elasticloadbalancingv2 DescribeTargetHealth output TargetHealthDescriptions[].{Target{Id,Port},TargetHealth{State,Reason,Description}}; ecs DescribeTasks output tasks[].containers[].healthStatus and tasks[].attachments[].details (privateIPv4Address).

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the three ids in trace_ids reproduce.
2. Create a VPC with two subnets and a security group; create an ECS cluster; register a Fargate awsvpc task definition with a container on port 80 and a container-level healthCheck.
3. Create an ALB and an ip-type target group in that VPC, plus a second target group that nothing references.
4. CreateService with desiredCount=1 and loadBalancers=[{targetGroupArn: <ALB group>, containerName, containerPort: 80}]; create an otherwise identical service with no loadBalancers.
5. Poll DescribeServices until runningCount==1 and DescribeTasks for containers[].healthStatus and the attachment's privateIPv4Address.
6. Poll elbv2 DescribeTargetHealth(TargetGroupArn=<ALB group>). Expected: exactly one target whose Id is the task's private IP and Port is 80, settling to State 'healthy'. Actual: the list stays empty and no state is ever published; the unreferenced group's empty list can therefore never be graded as a contrast.
7. Re-replay all three ids in trace_ids to confirm the fix.
