Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host|_poll_until#2|Timeout|last = probe()
Issue-Key: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults|run#18|AssertionError|assert sorted(got["compatibilities"]) == ["EC2", "FARGATE"], (
Issue-Commit: dd0dfa661ba492304c3b4ddf1bd869b805afe51f

# ecs: 2 net-new causes — a draining EC2 container instance never relocates its service task, and DescribeTaskDefinition derives MANAGED_INSTANCES into an awsvpc/FARGATE FireLens revision's compatibilities

> _Note: this issue was identified against the code base at commit `dd0dfa661ba492304c3b4ddf1bd869b805afe51f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterContainerInstance

## Description
2 causes, grounded in 2 failing checks the suite report lists under `### ecs`. Neither check is marked "ALREADY FILED" nor "OWNED by" an open issue, so the suite does not consider either covered.

Cause 1 of 2: a two-host ECS-on-EC2 estate never materialises, so draining a container instance never relocates its service task. Failing check: ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host.

  AWS: an EC2 instance launched from the ECS-optimized AMI with an instance profile carrying AmazonEC2ContainerServiceforEC2Role and user data `echo ECS_CLUSTER=<cluster> >> /etc/ecs/ecs.config` registers itself through RegisterContainerInstance, so ListContainerInstances(cluster) returns its containerInstanceArn and DescribeContainerInstances reports status ACTIVE / agentConnected true (Smithy: com.amazonaws.ecs#RegisterContainerInstance -> {containerInstance}; ListContainerInstances -> {containerInstanceArns, nextToken}; DescribeContainerInstances input {cluster, containerInstances (required), include} -> {containerInstances, failures}). With two such hosts and an EC2-launch-type service of desiredCount 1, UpdateContainerInstancesState(status=DRAINING) on the host running the task is accepted with failures == [], the service scheduler then STOPS that task with stopCode "ServiceSchedulerInitiated" (Smithy: Task$stopCode -> TaskStopCode, value ServiceSchedulerInitiated), starts a NEW task on the other container instance, the service returns to runningCount 1 / pendingCount 0, and the drained host stays status DRAINING with runningTasksCount 0 and pendingTasksCount 0. The model states the shapes; the scheduler timing is recorded behavior only, and in the recording it completes well inside the check's budget.

  Emulator: the check never gets past building the estate — it is cut by the harness with `Error: Check timed out after 60.0s` while polling for the two launched instances to appear as ACTIVE, agent-connected container instances. No error and no success is produced: the instance id RunInstances hands back is never joined to a container-instance record, so UpdateContainerInstancesState has no real subject, the relocation never happens and the stopCode/host assertions are never reached.

  Why: a client cannot drain a container instance for maintenance and watch its service workload move. Nothing scoped to a container instance can be exercised OR refused — the caller gets silence where AWS returns a 200 with a relocated task, so an operator script that drains a host and waits for runningCount to recover hangs forever.

  Fix: whatever stands an emulator-launched EC2 instance up as an ECS agent host and joins it to the cluster named in its user data, the ECS register_container_instance handler, and the container-instance records that ListContainerInstances/DescribeContainerInstances are served from; then the service scheduler's draining path, which must stop the drained host's task with stopCode ServiceSchedulerInitiated and place a replacement on another ACTIVE instance.

  Note for the fixer: open issue 20261120T023605Z-ecs-an-ec2-instance-launched-with-an-ecs.md describes the same container-instance registration gap and already names this check id among its traces, but the suite does not mark this check OWNED (its recorded replay key no longer matches the frame it now fails at), so it is reported here rather than dropped. Closing either issue requires replaying this check id; do not close one without re-running it.

  Traces: ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host.

Cause 2 of 2: DescribeTaskDefinition reports MANAGED_INSTANCES in the DERIVED compatibilities of an awsvpc task definition that requires only FARGATE. Failing check: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults.

  AWS: TaskDefinition$compatibilities is derived by ECS from the stored revision's own parameters (Smithy: com.amazonaws.ecs#TaskDefinition$compatibilities -> CompatibilityList of Compatibility, value set [EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES]; the member documents only "Amazon ECS validates the task definition parameters with those supported by the launch type", so the derivation rule itself is recorded behavior only). For a two-container FireLens revision with networkMode awsvpc, requiresCompatibilities ["FARGATE"], cpu "2048", memory "4096" and ephemeralStorage 40 GiB, the recorded AWS response is HTTP 200 with compatibilities == ["EC2", "FARGATE"] (sorted) — no MANAGED_INSTANCES entry — while requiresCompatibilities stays exactly ["FARGATE"].

  Emulator: the same DescribeTaskDefinition returns HTTP 200 with compatibilities ['EC2', 'MANAGED_INSTANCES', 'FARGATE'], i.e. MANAGED_INSTANCES is added to a revision whose parameters do not support that launch type.

  Why: a client that reads compatibilities to choose a launch type or capacity provider concludes this revision may be placed on ECS Managed Instances capacity; real AWS refuses that placement. A client that reads the list positionally reads the wrong launch type at index 1, and because the emulator reuses the derived list as its launchType gate, RunTask/CreateService with launchType MANAGED_INSTANCES is admitted for a revision AWS would reject.

  Fix: the task-definition projection's compatibilities derivation in the ECS service state — the MANAGED_INSTANCES arm of the per-launch-type predicate, which must be classified from the revision's own parameters the way the FARGATE and EXTERNAL arms are instead of holding for every revision; the launch-type validation helper that shares that predicate then stops admitting the launch type too.

  Note for the fixer: open issue 20261120T233305Z-ecs-servicediscovery-ecs-describetaskdefinition-reports-managed-instances-in-the-derived.md reports the same defect on a bridge/EC2 revision under service heading `ecs__servicediscovery`, and open wire issue 20261120T005727Z-ecs-task-definition-records-render-wrong-compatibilities-volumes.md touches the same member but asks for MANAGED_INSTANCES to be ADDED on its own fixtures. Neither carries this check, which is why it is filed here; one predicate fix should satisfy both, but this check id must be replayed to confirm it.

  Traces: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults.

## Evidence
Cause 1 — ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host (verdict: mismatch):

  Error: Check timed out after 60.0s

  The body builds a two-host estate (_two_host_estate), registers an EC2 task definition (one essential busybox container, cpu 64, memory 64, command ["sh","-c","sleep 1800"]), creates an EC2-launch-type service with desiredCount 1 and deploymentConfiguration {minimumHealthyPercent:100, maximumPercent:200}, then polls list_tasks/describe_tasks for the single RUNNING task. It is cut by the harness inside the estate/registration poll, so none of its assertions is reached.

  Expected (recorded real AWS), the assertions sitting past the gate:
    - UpdateContainerInstancesState(cluster, containerInstances=[<host running the task>], status="DRAINING") -> HTTP 200 with failures == [].
    - The service's single RUNNING task moves to the OTHER containerInstanceArn, is a NEW taskArn, and the service reports runningCount 1 / pendingCount 0.
    - The original task reaches lastStatus STOPPED with stopCode == "ServiceSchedulerInitiated" (the fixed enum value, no request or state values in it) and containerInstanceArn still the drained host.
    - DescribeContainerInstances on the drained host reports runningTasksCount 0, pendingTasksCount 0 and status "DRAINING".

  Actual (emulator): no container instance ever appears for the launched instances, so the drain, the relocation and the stopCode are never produced — neither a 200 nor a modeled error.

Cause 2 — ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults (verdict: mismatch):

  File "check_ecs_ecsdeploy_firelens_task_definition_round_trips_with_router_defaults.py", line 54, in run
    assert sorted(got["compatibilities"]) == ["EC2", "FARGATE"], (
  AssertionError: compatibilities are COMPUTED from the definition; got ['EC2', 'MANAGED_INSTANCES', 'FARGATE']

  Request: RegisterTaskDefinition(family=<unique>, networkMode="awsvpc", requiresCompatibilities=["FARGATE"], cpu="2048", memory="4096", ephemeralStorage={sizeInGiB:40}) with two containers — a non-essential FireLens router (firelensConfiguration {type:"fluentbit"}, memoryReservation 512, cpu 256) and an essential service container (logDriver "awsfirelens" with no options, portMappings [{containerPort:8443}], memory 4096, cpu 1792) — then DescribeTaskDefinition(taskDefinition=<arn>).

  Expected (recorded real AWS): HTTP 200, taskDefinition.compatibilities == ["EC2", "FARGATE"] (sorted), requiresCompatibilities == ["FARGATE"].
  Actual (emulator): HTTP 200, taskDefinition.compatibilities == ['EC2', 'MANAGED_INSTANCES', 'FARGATE'].

  The family name and image URIs are the run's generated values; the requirement is the contents of the derived list for an awsvpc/FARGATE revision, not those literals.

## Steps to Reproduce
Cause 1 (ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host):
1. CreateCluster, create an ECS instance role + instance profile carrying AmazonEC2ContainerServiceforEC2Role, and resolve the ECS-optimized AMI from SSM /aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id.
2. RunInstances twice (t3.micro) with that profile and user data `echo ECS_CLUSTER=<cluster> >> /etc/ecs/ecs.config`.
3. Poll ListContainerInstances(cluster): on the emulator it stays empty for the whole budget; on AWS two ARNs appear and DescribeContainerInstances reports each with the launched ec2InstanceId, status ACTIVE and agentConnected true.
4. RegisterTaskDefinition (one essential busybox container, cpu 64, memory 64, command sleep 1800) and CreateService with launchType EC2, desiredCount 1, deploymentConfiguration {minimumHealthyPercent:100, maximumPercent:200}; wait for one RUNNING task and note its containerInstanceArn.
5. UpdateContainerInstancesState(cluster, containerInstances=[that host], status="DRAINING") — expect 200 with failures [].
6. Poll: the task must be stopped with stopCode "ServiceSchedulerInitiated", a NEW task must be RUNNING on the other host, the service back to runningCount 1 / pendingCount 0, and the drained host must stay DRAINING with 0 running/pending tasks.

Cause 2 (ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults):
1. RegisterTaskDefinition with networkMode "awsvpc", requiresCompatibilities ["FARGATE"], cpu "2048", memory "4096", ephemeralStorage {sizeInGiB:40}, a non-essential FireLens router container (firelensConfiguration {type:"fluentbit"}) and an essential app container using the option-less awsfirelens log driver.
2. DescribeTaskDefinition on that ARN and read taskDefinition.compatibilities.
3. Observe ['EC2','MANAGED_INSTANCES','FARGATE']; real AWS derives only [EC2, FARGATE] for this revision while requiresCompatibilities stays ["FARGATE"].
4. Also confirm RunTask/CreateService with launchType MANAGED_INSTANCES is refused for this revision once the predicate is fixed.

Replay both check ids in trace_ids to confirm the fix.
