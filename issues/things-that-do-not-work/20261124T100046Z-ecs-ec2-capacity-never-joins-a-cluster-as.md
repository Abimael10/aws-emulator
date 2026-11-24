Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.runtask-without-capacity-rejected ecs.start-task-on-draining-container-instance-rejected ecs.e2e-capacity-provider-driven-scaling-flow
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.runtask-without-capacity-rejected|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: ecs.start-task-on-draining-container-instance-rejected|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-capacity-provider-driven-scaling-flow|_poll_until#59|Timeout|time.sleep(interval_s)
Issue-Commit: 0ed36ac69b61f04777ef4cd85714b039c0593841

# ecs: EC2 capacity never joins a cluster as a registered container instance — neither a directly launched agent host nor an ASG/capacity-provider-launched one — so RunTask/StartTask capacity behaviour is never reachable

> _Note: this issue was identified against the code base at commit `0ed36ac69b61f04777ef4cd85714b039c0593841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterContainerInstance

## Description
Two net-new root causes, grounded in 3 failing checks the suite report lists under `### ecs`, none of which is marked "ALREADY FILED" and none of which is marked "OWNED by" any open issue. Both are the ECS-on-EC2 container-instance registration gap, reached by two different entry paths.

Related open issue, which does NOT own these three checks (the report marks none of them OWNED by it): 20261120T023605Z-ecs-an-ec2-instance-launched-with-an-ecs.md covers the same registration gate for a different, disjoint set of 13 check ids. Closing it without replaying the three ids in `trace_ids` here leaves these three unrun, because each gates a DIFFERENT downstream behaviour (RunTask's zero-capacity refusal, StartTask's DRAINING placement failure, and the ASG-backed capacity-provider flow).

Cause 1 of 2: an EC2 instance launched directly with an ECS instance profile and `ECS_CLUSTER` user data never becomes a registered, ACTIVE, agent-connected container instance, so the two capacity-sensitive refusals behind that gate are never produced.
Failing check ids: ecs.runtask-without-capacity-rejected, ecs.start-task-on-draining-container-instance-rejected.

  AWS: `RunInstances` with an ECS-optimized AMI (resolved from SSM `/aws/service/ecs/optimized-ami/amazon-linux-2/recommended`), an instance profile carrying `AmazonEC2ContainerServiceforEC2Role`, and user data `echo ECS_CLUSTER=<cluster> >> /etc/ecs/ecs.config` makes the on-host agent call `RegisterContainerInstance`. `ListContainerInstances(cluster=<c>)` then returns one `containerInstanceArn`, and `DescribeContainerInstances` reports it with `ec2InstanceId` = the launched id, `status: "ACTIVE"`, `agentConnected: true`. Smithy (com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113): `RegisterContainerInstance` input {cluster, instanceIdentityDocument, instanceIdentityDocumentSignature, totalResources, versionInfo, containerInstanceArn, attributes, platformDevices, tags} -> output {containerInstance}, errors AccessDeniedException 403, ClientException/ClusterNotFoundException/InvalidParameterException 400, ServerException 500; `ListContainerInstances` -> {containerInstanceArns, nextToken}; `DescribeContainerInstances` input {cluster, containerInstances (required), include} -> {containerInstances, failures}. The model states the shapes, not the timing — the timing is recorded behavior only: in the recording the instance registers well inside each check's budget. Once registered, AWS's behaviours these two checks grade are:
    - `RunTask` with `launchType: EC2` against a cluster whose only container instance has been force-deregistered is refused HTTP 400 with `Error.Code = "InvalidParameterException"` and `Error.Message = "No Container Instances were found in your cluster."` — the whole message is AWS's fixed text for this error class and carries no request or state value — and it creates no task (`ListTasks` stays empty). The identical call succeeds (200, `tasks[0]`, `failures: []`) both before the deregistration and again after fresh capacity registers, so the refusal tracks capacity rather than being unconditional. Smithy: `RunTask` models InvalidParameterException at httpError 400, output {tasks, failures}.
    - `StartTask` naming a container instance that has been moved to `DRAINING` by `UpdateContainerInstancesState` is NOT an exception: it is HTTP 200 with `tasks: []` and exactly one `failures[]` entry whose `arn` names that container instance (StartTask's failure record uses the short ARN form, without the cluster-name segment) and whose `reason` is non-empty. The DRAINING instance must then still describe with `runningTasksCount: 0` and `pendingTasksCount: 0`. Returning the instance to `ACTIVE` must make the identical `StartTask` succeed with one task and `failures: []`. Smithy: `StartTask` input {containerInstances (required), taskDefinition (required), ...} -> output {tasks, failures}; `UpdateContainerInstancesState` input {containerInstances (required), status (required: ContainerInstanceStatus)} -> {containerInstances, failures}.

  Emulator: the registration never happens. Both bodies launch the instance, then poll `ListContainerInstances` / `DescribeContainerInstances` for a container instance whose `ec2InstanceId` equals the launched id with `agentConnected: true` and `status: "ACTIVE"`; the list stays empty for the whole budget and the harness cuts the check: `Error: Check timed out after 60.0s` for both ids. Neither an error nor a success is produced for the registration — the instance id the emulator hands back from `RunInstances` is never joined to any container-instance record. As a consequence neither capacity-sensitive behaviour above is ever exercised: the emulator is never asked for the zero-capacity `InvalidParameterException` or for StartTask's DRAINING `failures[]` entry.

  Why: a client cannot distinguish "this cluster has capacity" from "this cluster has none" on the emulator. `RunTask` with `launchType: EC2` has no subject to refuse, so a client that relies on the 400 `InvalidParameterException` / `No Container Instances were found in your cluster.` contract to detect an empty cluster gets silence, and a client that relies on StartTask's per-instance `failures[]` entry to learn that a host is draining sees nothing to inspect — draining is unobservable, so a drain-before-terminate workflow cannot be written or tested against the emulator at all.

  Fix: whatever stands an emulator-launched EC2 instance up as an ECS agent host and joins it to the cluster named in its user data, plus the ECS `register_container_instance` handler and the container-instance records in `crate::state` that `ListContainerInstances` / `DescribeContainerInstances` read; then the capacity checks in the `run_task` handler (the zero-capacity refusal) and the per-instance placement-failure path in the `start_task` handler (DRAINING candidate -> `failures[]`, not a task and not a raised error).

  Traces: ecs.runtask-without-capacity-rejected, ecs.start-task-on-draining-container-instance-rejected.

Cause 2 of 2: scaling an ASG that backs an ECS capacity provider produces no registered container instance, so the capacity-provider-driven placement flow never starts.
Failing check id: ecs.e2e-capacity-provider-driven-scaling-flow.

  AWS: with a launch template carrying the ECS-optimized AMI, the ECS instance profile and `ECS_CLUSTER=<cluster>` user data, an ASG at DesiredCapacity 0, a capacity provider whose `autoScalingGroupProvider.autoScalingGroupArn` is that ASG (`managedScaling: DISABLED`, `managedTerminationProtection: DISABLED`), and that provider attached to the cluster via `PutClusterCapacityProviders` as the default strategy, `UpdateAutoScalingGroup(DesiredCapacity=1)` launches an instance that self-registers: `ListContainerInstances(cluster=<c>, status="ACTIVE")` returns at least one ARN. A service created with `capacityProviderStrategy=[{capacityProvider: <cp>, weight: 1, base: 0}]` and `desiredCount: 1` then reaches `runningCount: 1`, and `DescribeContainerInstances` on the registered instance reports `runningTasksCount >= 1`. Afterwards every resource created (service, capacity provider, ASG, launch template, IAM role, cluster) tears down. Recorded behavior only for the timing; the model states the shapes: `ListContainerInstances` -> {containerInstanceArns, nextToken}, `DescribeContainerInstances` -> {containerInstances, failures}.

  Emulator: after `UpdateAutoScalingGroup(DesiredCapacity=1)` the poll of `ListContainerInstances(cluster=<c>, status="ACTIVE")` never returns a single ARN, and the check is cut by the harness: `Error: Check timed out after 60.0s`. Nothing downstream is reached — the service is never created, so `runningCount` and `runningTasksCount` are never compared, and the teardown sequence is never graded. The ASG-launched instance, unlike the directly launched one in Cause 1, is additionally never observed to exist as ECS capacity at all, so this path has its own gap on top of the registration gap: the capacity provider's ASG association does not translate a desired-capacity change into ECS capacity.

  Why: a client cannot use capacity providers for what they exist for. Attaching an ASG-backed provider and raising desired capacity never yields placeable capacity, so a service with a `capacityProviderStrategy` can never reach steady state on the emulator and a client that waits for `runningCount == desiredCount` waits forever instead of receiving either the running service or a placement failure.

  Fix: the capacity-provider -> AutoScaling-group association path (whatever translates an ASG desired-capacity change into launched instances) and the same agent-registration path and container-instance state as Cause 1, then the capacity-provider-strategy branch of the service scheduler that places the task on the resulting instance and the `runningTasksCount` field served by `DescribeContainerInstances`.

  Traces: ecs.e2e-capacity-provider-driven-scaling-flow.

## Evidence
Cause 1 — expected (recorded AWS):
  1. `ec2 run-instances --image-id <ecs-optimized-ami> --instance-type t3.micro --iam-instance-profile Name=<ecs-instance-profile> --subnet-id <s> --security-group-ids <sg> --user-data $(base64 <<< 'echo ECS_CLUSTER=<c> >> /etc/ecs/ecs.config')`, then `ecs list-container-instances --cluster <c>` returns one ARN and `ecs describe-container-instances --cluster <c> --container-instances <ci>` reports `ec2InstanceId` = the launched id, `status: "ACTIVE"`, `agentConnected: true`.
  2. `ecs run-task --cluster <c> --task-definition <td> --launch-type EC2 --count 1` -> HTTP 200, one task, `failures: []`.
  3. `ecs deregister-container-instance --cluster <c> --container-instance <ci> --force`, then the identical run-task -> HTTP 400, `Error.Code = "InvalidParameterException"`, `Error.Message = "No Container Instances were found in your cluster."` (entire message fixed text; no request/state values), and `ecs list-tasks --cluster <c>` -> `taskArns: []`.
  4. Register fresh capacity; the identical run-task -> HTTP 200 with one task again.
  5. `ecs start-task --cluster <c> --container-instances <ci> --task-definition <td>` while ACTIVE -> 200, one task, `failures: []`. After `ecs update-container-instances-state --cluster <c> --container-instances <ci> --status DRAINING` -> 200 with `tasks: []` and exactly one `failures[]` entry naming that instance (short-ARN form) with a non-empty `reason`; `describe-container-instances` then shows `runningTasksCount: 0`, `pendingTasksCount: 0`. After `--status ACTIVE` -> the identical start-task is 200 with one task and `failures: []`.

Cause 1 — actual (emulator), both checks cut inside the registration poll so no assertion past it is reached:
  Error: Check timed out after 60.0s   (ecs.runtask-without-capacity-rejected)
  Error: Check timed out after 60.0s   (ecs.start-task-on-draining-container-instance-rejected)
The poll that never converges, verbatim from the bodies:
  ecs.runtask-without-capacity-rejected: `_poll_until(_active_instances, lambda arns: len(arns) >= 1, timeout_s=480.0, interval_s=15.0)` where `_active_instances()` is `ecs.list_container_instances(cluster=cluster_arn, status="ACTIVE")["containerInstanceArns"]`.
  ecs.start-task-on-draining-container-instance-rejected: `_poll_until(lambda: _find_container_instance(ecs, cluster_arn, instance_id), lambda arn: arn is not None, timeout_s=420, interval_s=15, description="EC2 instance to register as an ECS container instance")`, where `_find_container_instance` requires `ci["ec2InstanceId"] == instance_id and ci["agentConnected"] and ci["status"] == "ACTIVE"`.
Assertions sitting past the gate, never evaluated:
  `assert message == "No Container Instances were found in your cluster."` (after `_expect_error(_run_task, "InvalidParameterException")`)
  `assert ecs.list_tasks(cluster=cluster_arn)["taskArns"] == []`
  `assert resp["tasks"] == []` / `assert len(resp["failures"]) == 1` / `assert failure.get("reason")` for the DRAINING StartTask
  `assert described["runningTasksCount"] == 0` and `assert described["pendingTasksCount"] == 0`

Cause 2 — expected (recorded AWS): after `autoscaling update-auto-scaling-group --auto-scaling-group-name <asg> --desired-capacity 1 --min-size 0` on the ASG referenced by the cluster's capacity provider, `ecs list-container-instances --cluster <c> --status ACTIVE` returns >= 1 ARN; `ecs create-service --cluster <c> --service-name <svc> --task-definition <td> --desired-count 1 --capacity-provider-strategy capacityProvider=<cp>,weight=1,base=0` then reaches `runningCount: 1`, and the registered container instance reports `runningTasksCount >= 1`.

Cause 2 — actual (emulator):
  Error: Check timed out after 60.0s   (ecs.e2e-capacity-provider-driven-scaling-flow)
cut inside `instances = _poll_until(_registered_instances, lambda arns: len(arns) >= 1, timeout_s=480.0, interval_s=15.0)`, whose probe is `ecs.list_container_instances(cluster=cluster_name, status="ACTIVE")["containerInstanceArns"]`. The list never becomes non-empty, so `create_service`, the `runningCount == 1` poll and the `runningTasksCount >= 1` poll are never reached.

(Generated instance ids, cluster/ASG/launch-template names and ARNs are per-run values and are not part of the requirement; the requirement is that SOME ACTIVE, agent-connected container instance carrying the launched instance id appears, and that the refusals above are then produced.)

## Steps to Reproduce
Cause 1 (ecs.runtask-without-capacity-rejected, ecs.start-task-on-draining-container-instance-rejected):
1. `ecs create-cluster --cluster-name <c>`; `ecs register-task-definition --family <f> --container-definitions '[{"name":"app","image":"public.ecr.aws/docker/library/busybox:stable","cpu":64,"memory":128,"essential":true,"command":["sh","-c","sleep 3600"]}]'`.
2. Create an ECS instance role + instance profile carrying `arn:aws:iam::aws:policy/service-role/AmazonEC2ContainerServiceforEC2Role`.
3. `ssm get-parameter --name /aws/service/ecs/optimized-ami/amazon-linux-2/recommended` and take `image_id` from the JSON value.
4. `ec2 run-instances --image-id <ami> --instance-type t3.micro --iam-instance-profile Name=<profile> --subnet-id <subnet> --security-group-ids <sg> --user-data $(base64 <<< 'echo ECS_CLUSTER=<c> >> /etc/ecs/ecs.config')`.
5. Poll `ecs list-container-instances --cluster <c> --status ACTIVE`: on the emulator it stays empty for the whole budget. On AWS it returns one ARN whose `describe-container-instances` entry has `ec2InstanceId` = the id from step 4, `status: ACTIVE`, `agentConnected: true`. This is where both checks die.
6. With a registered instance in hand, the gated behaviours to verify after the fix:
   a. `ecs run-task --cluster <c> --task-definition <td> --launch-type EC2 --count 1` -> 200, one task; stop it.
   b. `ecs deregister-container-instance --cluster <c> --container-instance <ci> --force`; `ecs list-container-instances --cluster <c> --status ACTIVE` -> `[]`; repeat (a) -> HTTP 400 `InvalidParameterException` / `No Container Instances were found in your cluster.`; `ecs list-tasks --cluster <c>` -> `[]`.
   c. Launch a second agent instance, let it register, repeat (a) -> 200 with one task again.
   d. `ecs start-task --cluster <c> --container-instances <ci> --task-definition <td>` while ACTIVE -> 200, one task, `failures: []`; stop it.
   e. `ecs update-container-instances-state --cluster <c> --container-instances <ci> --status DRAINING`; poll describe until `status == DRAINING`; repeat (d) -> 200 with `tasks: []` and exactly one `failures[]` entry whose `arn` ends in the same container-instance id and whose `reason` is non-empty; `describe-container-instances` -> `runningTasksCount: 0`, `pendingTasksCount: 0`.
   f. `ecs update-container-instances-state ... --status ACTIVE`; poll until ACTIVE; repeat (d) -> 200 with one task and `failures: []`.

Cause 2 (ecs.e2e-capacity-provider-driven-scaling-flow):
1. `ecs create-cluster --cluster-name <c>`.
2. `ec2 create-launch-template --launch-template-name <lt>` with `ImageId=<ecs-optimized-ami>`, `InstanceType=t3.micro`, `IamInstanceProfile={Name:<ecs-profile>}`, `SecurityGroupIds=[<sg>]`, `UserData=$(base64 <<< 'echo ECS_CLUSTER=<c> >> /etc/ecs/ecs.config')`.
3. `autoscaling create-auto-scaling-group --auto-scaling-group-name <asg> --launch-template LaunchTemplateName=<lt>,Version='$Latest' --min-size 0 --max-size 2 --desired-capacity 0 --vpc-zone-identifier <subnet>`; read `AutoScalingGroupARN`.
4. `ecs create-capacity-provider --name <cp> --auto-scaling-group-provider autoScalingGroupArn=<asgArn>,managedScaling={status=DISABLED},managedTerminationProtection=DISABLED`.
5. `ecs put-cluster-capacity-providers --cluster <c> --capacity-providers <cp> --default-capacity-provider-strategy capacityProvider=<cp>,weight=1,base=0`.
6. `autoscaling update-auto-scaling-group --auto-scaling-group-name <asg> --desired-capacity 1 --min-size 0`.
7. Poll `ecs list-container-instances --cluster <c> --status ACTIVE`: on the emulator it never becomes non-empty (check dies here). On AWS it returns >= 1 ARN.
8. After the fix: `ecs create-service --cluster <c> --service-name <svc> --task-definition <td> --desired-count 1 --capacity-provider-strategy capacityProvider=<cp>,weight=1,base=0` -> poll `describe-services` to `runningCount: 1`; `describe-container-instances` on the registered ARN -> `runningTasksCount >= 1`; then tear down service, capacity provider, ASG, launch template, IAM role and cluster.
Replay: ecs.runtask-without-capacity-rejected, ecs.start-task-on-draining-container-instance-rejected, ecs.e2e-capacity-provider-driven-scaling-flow.
