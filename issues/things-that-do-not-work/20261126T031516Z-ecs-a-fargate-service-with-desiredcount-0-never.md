Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.concurrent-update-task-protection-conflicting-expiry ecs.continue-service-deployment-advances-waiting-deployment ecs.e2e-service-deploy-scale-drain-teardown ecs.e2e-service-deploy-scale-observe-teardown ecs.task-protection-ttl-renewal-resets-timer ecs.update-service-desired-count-keeps-the-primary-deployment
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.concurrent-update-task-protection-conflicting-expiry|_wait_for_running_task#8|Timeout|time.sleep(5)
Issue-Key: ecs.continue-service-deployment-advances-waiting-deployment|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-service-deploy-scale-drain-teardown|_poll_until#24|TimeoutError|raise TimeoutError("the polled condition: the run budget ended first; not graded")
Issue-Key: ecs.e2e-service-deploy-scale-observe-teardown|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-ttl-renewal-resets-timer|_wait_for_running_task#8|Timeout|time.sleep(5)
Issue-Key: ecs.update-service-desired-count-keeps-the-primary-deployment|_settled#17|Timeout|time.sleep(5.0)
Issue-Commit: 01f237389f2ba129bcfe6809f1b810fd15e9edea

# ecs: a Fargate service with desiredCount>0 never produces a RUNNING task, so six service-scheduler / task-protection / deployment checks never reach their assertions

> _Note: this issue was identified against the code base at commit `01f237389f2ba129bcfe6809f1b810fd15e9edea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
1 net-new root cause, failing six checks of the ecs service group that are NOT carried by any open issue's replay keys.

Cause 1 of 1: a service created with launchType=FARGATE and desiredCount>0 never reports a task in lastStatus RUNNING, and the service/deployment never reports runningCount == desiredCount, so every check that must first get a running service task is cut off by the harness budget before it can assert the behaviour it exists for.

AWS: Smithy: ecs Service.runningCount — 'The number of tasks in the cluster that are in the RUNNING state'; Service.pendingCount — 'The number of tasks in the cluster that are in the PENDING state'; Deployment.runningCount/desiredCount and Deployment.rolloutState — 'When a service deployment is started, it begins in an IN_PROGRESS state. When the service reaches a steady state, the deployment transitions to a COMPLETED state.' On AWS a REPLICA service on FARGATE places desiredCount tasks that transition PROVISIONING -> PENDING -> RUNNING; ListTasks(desiredStatus=RUNNING) then returns their ARNs and DescribeTasks reports lastStatus=RUNNING, and the service's runningCount rises to desiredCount with pendingCount 0 and exactly one deployment in rolloutState COMPLETED. The fixed requirement is the convergence itself (a steady state is reached and reported), not any particular timing of a single run.

Emulator: none of the six checks ever observes that steady state. Five are killed by the harness 60 s ceiling ('Check timed out after 60.0s') while polling for it, and ecs.e2e-service-deploy-scale-drain-teardown ends 'TimeoutError: the polled condition: the run budget ended first; not graded' inside its runningCount poll. The polls that never satisfy are: ListTasks(serviceName, desiredStatus=RUNNING) + DescribeTasks for a task with lastStatus RUNNING (ecs.concurrent-update-task-protection-conflicting-expiry, ecs.task-protection-ttl-renewal-resets-timer), DescribeServices until runningCount==desiredCount and pendingCount==0 (ecs.e2e-service-deploy-scale-observe-teardown at desiredCount 2 then 4, ecs.e2e-service-deploy-scale-drain-teardown, ecs.continue-service-deployment-advances-waiting-deployment's pre-step at desiredCount 1), and DescribeServices until status ACTIVE with runningCount==N and rolloutState COMPLETED (ecs.update-service-desired-count-keeps-the-primary-deployment). No HTTP status or error code diverges: CreateService/UpdateService answer 200 and the reads answer 200 with a service that never converges.

Why: a client cannot use the emulator for anything that requires a running service task. Everything these checks exist to verify is unreachable behind this one gap and so is unverified by the emulator today: UpdateTaskProtection's expirationDate reset on renewal (Smithy: UpdateTaskProtection.expiresInMinutes — '1 minute to up to 2,880 minutes', ProtectedTask.expirationDate 'The epoch time when protection for the task will expire'), last-writer-wins resolution of two concurrent UpdateTaskProtection calls with GetTaskProtection stable across reads, the BLUE_GREEN PRE_SCALE_UP PAUSE lifecycle hook reaching AWAITING_ACTION and ContinueServiceDeployment(action=CONTINUE) driving it to SUCCEEDED, UpdateService desiredCount keeping the single PRIMARY deployment (same id and createdAt, no new ServiceDeployment record), and the scale/drain/teardown walk down to INACTIVE. A fixer must re-replay all six, because the assertions behind the convergence are each distinct requirements that have never been exercised.

Fix: the ECS service scheduler / task lifecycle component — the part that, for an ACTIVE REPLICA service, places desiredCount tasks, advances each task to RUNNING and keeps the service's and its deployment's runningCount/pendingCount/rolloutState in step with the placed tasks. Related open issues describe the same area but do not carry these checks' replay keys and must not be assumed to cover them: 20261125T221450Z-ecs-the-service-scheduler-never-converges-5-net.md and 20261125T221450Z-ecs-the-task-data-plane-never-produces-a.md.

Traces: ecs.concurrent-update-task-protection-conflicting-expiry, ecs.continue-service-deployment-advances-waiting-deployment, ecs.e2e-service-deploy-scale-drain-teardown, ecs.e2e-service-deploy-scale-observe-teardown, ecs.task-protection-ttl-renewal-resets-timer, ecs.update-service-desired-count-keeps-the-primary-deployment.

## Evidence
Expected (AWS): after CreateService(launchType=FARGATE, desiredCount=N, awsvpc networkConfiguration) the service converges — ListTasks(desiredStatus=RUNNING) returns N task ARNs whose DescribeTasks lastStatus is RUNNING, DescribeServices reports runningCount=N, pendingCount=0 and one deployment with rolloutState COMPLETED; UpdateService(desiredCount=M) converges the same way without creating a second deployment record.

Actual (emulator):
- ecs.concurrent-update-task-protection-conflicting-expiry: 'Check timed out after 60.0s' inside _wait_for_running_task (ListTasks+DescribeTasks for lastStatus RUNNING) for a desiredCount=1 FARGATE service.
- ecs.task-protection-ttl-renewal-resets-timer: 'Check timed out after 60.0s' in the same _wait_for_running_task poll.
- ecs.e2e-service-deploy-scale-observe-teardown: 'Check timed out after 60.0s' polling DescribeServices for runningCount==2 and pendingCount==0 (desiredCount=2 at create).
- ecs.e2e-service-deploy-scale-drain-teardown: 'File "check_ecs_e2e_service_deploy_scale_drain_teardown.py", line 86, in run / scaled_up = _poll_until(... ) / TimeoutError: the polled condition: the run budget ended first; not graded'.
- ecs.update-service-desired-count-keeps-the-primary-deployment: 'Check timed out after 60.0s'; its _settled() helper requires status ACTIVE, runningCount==N and rollouts == ['COMPLETED'].
- ecs.continue-service-deployment-advances-waiting-deployment: 'Check timed out after 60.0s'; its first wait is DescribeServices until len(deployments)==1 and deployments[0].runningCount == desiredCount == 1, before any BLUE_GREEN/ContinueServiceDeployment step is attempted.
Cluster names, service names, task ARNs and the harness port differ per run and are not the requirement; the invariant that fails is that no service task ever reaches RUNNING and no service ever reports runningCount == desiredCount.

## Steps to Reproduce
1. Point an AWS client at the emulator. CreateCluster, then RegisterTaskDefinition(requiresCompatibilities=[FARGATE], networkMode=awsvpc, cpu=256, memory=512, one essential container running `sleep 3600`).
2. CreateService(cluster, serviceName, taskDefinition, desiredCount=1, launchType=FARGATE, networkConfiguration.awsvpcConfiguration with the shared VPC's subnets): the call answers 200 with status ACTIVE.
3. Poll ListTasks(cluster, serviceName, desiredStatus=RUNNING) and DescribeTasks on the result: no task ever reports lastStatus RUNNING.
4. Poll DescribeServices(cluster, [serviceName]): runningCount stays below desiredCount and the deployment never reaches rolloutState COMPLETED; UpdateService(desiredCount=4) does not converge either.
5. Re-replay all six checks to confirm the fix: ecs.concurrent-update-task-protection-conflicting-expiry, ecs.continue-service-deployment-advances-waiting-deployment, ecs.e2e-service-deploy-scale-drain-teardown, ecs.e2e-service-deploy-scale-observe-teardown, ecs.task-protection-ttl-renewal-resets-timer, ecs.update-service-desired-count-keeps-the-primary-deployment.
