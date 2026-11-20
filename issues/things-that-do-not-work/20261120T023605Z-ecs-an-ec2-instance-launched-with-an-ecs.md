Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.concurrent-container-instance-state-update ecs.container-instance-register-state-deregister ecs.container-state-change-sequencing ecs.deep-ecs-2-ec2place-binpack-colocates-while-spread-separates ecs.deep-ecs-2-ec2place-bridge-dynamic-host-port-serves-sibling-and-fixed-port-collision-rejected ecs.deep-ecs-2-ec2place-distinct-instance-third-task-on-two-hosts-rejected ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host ecs.placement-constraint-memberof-attribute-filters-candidate-instances ecs.poll-endpoint-requires-registered-instance ecs.put-attributes-exceeding-limit-rejected ecs.start-task-batch-partial-failure ecs.task-state-change-ordering-enforced ecs.update-container-instances-state-unknown
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.concurrent-container-instance-state-update|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: ecs.container-instance-register-state-deregister|_poll_until#8|Timeout|time.sleep(interval_s)
Issue-Key: ecs.container-state-change-sequencing|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deep-ecs-2-ec2place-binpack-colocates-while-spread-separates|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deep-ecs-2-ec2place-bridge-dynamic-host-port-serves-sibling-and-fixed-port-collision-rejected|_poll_until#3|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: ecs.deep-ecs-2-ec2place-distinct-instance-third-task-on-two-hosts-rejected|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: ecs.placement-constraint-memberof-attribute-filters-candidate-instances|_poll_until#4|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: ecs.poll-endpoint-requires-registered-instance|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: ecs.put-attributes-exceeding-limit-rejected|_poll_until#4|Timeout|time.sleep(interval_s)
Issue-Key: ecs.start-task-batch-partial-failure|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-state-change-ordering-enforced|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: ecs.update-container-instances-state-unknown|_poll_until#3|Timeout|time.sleep(interval_s)
Issue-Commit: 15f808f6512aed05afde1caaa5d6c2b7e80884c0

# ecs: an EC2 instance launched with an ECS instance profile and ECS_CLUSTER user data never becomes a registered ACTIVE agent-connected container instance, so all 13 ECS-on-EC2 bodies burn their budget at that first gate

> _Note: this issue was identified against the code base at commit `15f808f6512aed05afde1caaa5d6c2b7e80884c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterContainerInstance

## Description
One root cause, grounded in 13 failing checks the suite report lists under `### ecs`, none of which is marked "ALREADY FILED" or "OWNED by" any open issue.

Cause 1 of 1: an emulator-launched EC2 instance never appears as a container instance of the cluster named in its user data, so every check whose first step is that registration never reaches the behavior it was written to grade.

  AWS: `RunInstances` with an ECS-optimized AMI (resolved from SSM `/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id`), an instance profile carrying `AmazonEC2ContainerServiceforEC2Role`, and user data `echo ECS_CLUSTER=<cluster> >> /etc/ecs/ecs.config` causes the on-host ECS agent to call `RegisterContainerInstance`. `ListContainerInstances(cluster=<cluster>)` then returns that instance's `containerInstanceArn`, and `DescribeContainerInstances` reports it with `ec2InstanceId` equal to the launched instance id, `status: ACTIVE` and `agentConnected: true`. Smithy (com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113): `RegisterContainerInstance` input {cluster, instanceIdentityDocument, instanceIdentityDocumentSignature, totalResources, versionInfo, containerInstanceArn, attributes, platformDevices, tags} -> output {containerInstance}, errors AccessDeniedException 403, ClientException/ClusterNotFoundException/InvalidParameterException 400, ServerException 500; `ListContainerInstances` -> {containerInstanceArns, nextToken}; `DescribeContainerInstances` input {cluster, containerInstances (required), include} -> {containerInstances, failures}. The model states the shapes, not the timing — the timing is recorded behavior only: in the recording the instance becomes an ACTIVE, agent-connected container instance well inside every one of these checks' budgets.

  Emulator: the registration never happens. Each body polls `ecs.list_container_instances` / `ecs.describe_container_instances` for a container instance whose `ec2InstanceId` is the launched id and is cut by the budget. Two of the thirteen reach their own poll deadline first and say so verbatim: `TimeoutError: EC2 instance i-00000000000000001 registers as an ACTIVE ECS container instance: the run budget ended first; not graded` (ecs.placement-constraint-memberof-attribute-filters-candidate-instances) and `TimeoutError: EC2 instances ['i-00000000000000001', 'i-00000000000000002'] register as ACTIVE, connected container instances: the run budget ended first; not graded` (ecs.deep-ecs-2-ec2place-bridge-dynamic-host-port-serves-sibling-and-fixed-port-collision-rejected). The other eleven are cut by the harness: `Error: Check timed out after 60.0s`. No error and no success is ever produced for the registration — the instance id the emulator hands back from RunInstances is simply never joined to any container-instance record.

  Why: the entire ECS-on-EC2 plane is unobservable and unfalsifiable for a client. Nothing scoped to a container instance can be exercised OR refused, so these distinct AWS behaviors all become silence instead of a 200 or a modeled 400:
    - container-instance lifecycle: register ACTIVE and listed, `UpdateContainerInstancesState` -> `DRAINING` as described, `DeregisterContainerInstance` -> unlisted but still described `INACTIVE`;
    - `UpdateContainerInstancesState` on an id that was never registered must be refused (and must not disturb a real instance's state) rather than claiming to drain it;
    - `PutAttributes` beyond the documented 10-attribute-per-resource ceiling must be refused HTTP 400 `AttributeLimitExceededException` with the fixed message `Attribute per target limit exceeded.` (whole text fixed, no request values) and must not apply the attribute, and the identical call must then succeed once a slot is freed (Smithy: PutAttributes models AttributeLimitExceededException and TargetNotFoundException at httpError 400);
    - `DiscoverPollEndpoint` must require a registered instance;
    - `StartTask` batch partial failure: a multi-instance batch must return per-entry `failures[]` alongside the placed `tasks[]` (Smithy: StartTask input `containerInstances` required, output {tasks, failures});
    - EC2 task placement: `binpack(memory)` co-locating two tasks on ONE host while `spread(instanceId)` uses BOTH; `distinctInstance` refusing the third task on two hosts with `failures[].reason = "DistinctInstance placement constraint unsatisfied."`; a `memberOf attribute:<name> == <value>` constraint over a custom attribute filtering candidates; a bridge-mode dynamic host port being reachable from a sibling host while a fixed host-port collision is rejected; draining a host making the service scheduler stop its task with `stopCode: ServiceSchedulerInitiated` and start a replacement on the other host;
    - agent-side state-change ordering: a stale `SubmitTaskStateChange(status=RUNNING)` must not regress a task already recorded STOPPED, and container state changes must sequence;
    - concurrent `UpdateContainerInstancesState` on one instance must settle to a single consistent status.
  A client gets neither an error nor a result for any of these — the calls simply never have a subject to act on.

  Fix: whatever stands an emulator-launched EC2 instance up as an ECS agent host and joins it to the cluster named in its user data, plus the ECS `register_container_instance` handler and the container-instance records in `crate::state` that `ListContainerInstances` / `DescribeContainerInstances` are served from. Once an instance registers, the behaviors listed under "Why" are what the thirteen checks then grade.

  Related open issues on the same registration gap, NONE of which owns these thirteen checks (the report marks none of them OWNED, because their recorded replay keys no longer match the frame they now fail at): 20261109T061533Z-ecs-5-net-new-causes-ec2-hosts-never.md (its cause 1), 20261109T083521Z-ecs-ec2-instances-launched-with-ecs-cluster-user.md, 20261109T013300Z-ec2-ecs-servicediscovery-an-ec2-instance-launched-from.md, 20261111T033241Z-ec2-ecs-events-sqs-an-ec2-instance-launched.md. Closing any of those without replaying the thirteen ids in `trace_ids` leaves them unrun.

  Traces: ecs.concurrent-container-instance-state-update, ecs.container-instance-register-state-deregister, ecs.container-state-change-sequencing, ecs.deep-ecs-2-ec2place-binpack-colocates-while-spread-separates, ecs.deep-ecs-2-ec2place-bridge-dynamic-host-port-serves-sibling-and-fixed-port-collision-rejected, ecs.deep-ecs-2-ec2place-distinct-instance-third-task-on-two-hosts-rejected, ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host, ecs.placement-constraint-memberof-attribute-filters-candidate-instances, ecs.poll-endpoint-requires-registered-instance, ecs.put-attributes-exceeding-limit-rejected, ecs.start-task-batch-partial-failure, ecs.task-state-change-ordering-enforced, ecs.update-container-instances-state-unknown.

## Evidence
Expected (recorded AWS): after `ec2 run-instances --image-id <ecs-optimized-ami> --iam-instance-profile Arn=<ecs-instance-profile> --subnet-id <s> --security-group-ids <sg> --user-data <base64 of 'echo ECS_CLUSTER=<c> >> /etc/ecs/ecs.config'>`, `ecs list-container-instances --cluster <c>` returns one `containerInstanceArn`, and `ecs describe-container-instances --cluster <c> --container-instances <ci>` reports `ec2InstanceId` = the launched instance id, `status: "ACTIVE"`, `agentConnected: true` — inside each check's budget.

Actual (emulator), the two checks whose own poll deadline fires before the harness cut, showing the gate verbatim:

  File "check_ecs_placement_constraint_memberof_attribute_filters_candidate_instances.py", line 436, in _launch_container_instance
    ci = _poll_until(
  File "check_ecs_placement_constraint_memberof_attribute_filters_candidate_instances.py", line 257, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: EC2 instance i-00000000000000001 registers as an ACTIVE ECS container instance: the run budget ended first; not graded

  File "check_ecs_deep_ecs_2_ec2place_bridge_dynamic_host_port_serves_sibling_and_fixed_port_collision_rejected.py", line 660, in _launch_hosts
    cis = _poll_until(
  File "check_ecs_deep_ecs_2_ec2place_bridge_dynamic_host_port_serves_sibling_and_fixed_port_collision_rejected.py", line 328, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: EC2 instances ['i-00000000000000001', 'i-00000000000000002'] register as ACTIVE, connected container instances: the run budget ended first; not graded

(The `i-0000000000000000N` instance ids are per-run generated values, not part of the requirement; the requirement is that SOME container instance appears carrying the launched id.)

Actual (emulator), the remaining eleven, all cut by the harness inside the same `_launch_real_container_instance` / `_two_host_estate` registration poll, so none of their assertions is ever reached:

  Error: Check timed out after 60.0s   (ecs.concurrent-container-instance-state-update)
  Error: Check timed out after 60.0s   (ecs.container-instance-register-state-deregister)
  Error: Check timed out after 60.0s   (ecs.container-state-change-sequencing)
  Error: Check timed out after 60.0s   (ecs.deep-ecs-2-ec2place-binpack-colocates-while-spread-separates)
  Error: Check timed out after 60.0s   (ecs.deep-ecs-2-ec2place-distinct-instance-third-task-on-two-hosts-rejected)
  Error: Check timed out after 60.0s   (ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host)
  Error: Check timed out after 60.0s   (ecs.poll-endpoint-requires-registered-instance)
  Error: Check timed out after 60.0s   (ecs.put-attributes-exceeding-limit-rejected)
  Error: Check timed out after 60.0s   (ecs.start-task-batch-partial-failure)
  Error: Check timed out after 60.0s   (ecs.task-state-change-ordering-enforced)
  Error: Check timed out after 60.0s   (ecs.update-container-instances-state-unknown)

Behaviors that are consequently never produced, each an assertion sitting past the gate:
  - `ecs put-attributes --cluster <c> --attributes name=one-too-many,targetId=<ci>,value=v` after ten attributes are already set must be HTTP 400 `Error.Code = "AttributeLimitExceededException"`, `Error.Message = "Attribute per target limit exceeded."` (the whole message is AWS's fixed text — it carries no request or state value) and must not apply the attribute; the identical call must succeed after one slot is freed.
  - `ecs update-container-instances-state --cluster <c> --container-instances <ci> --status DRAINING` against a genuinely registered instance must be HTTP 200 with `failures: []` and `containerInstances[0].status == "DRAINING"`; the same call naming an id that was never registered must be refused and must leave the real instance's status untouched.
  - `distinctInstance` over two container instances with `--count 3` must return a `failures[]` entry whose `reason` is the fixed text `DistinctInstance placement constraint unsatisfied.`
  - draining a host must make the service scheduler stop its task with `stopCode: "ServiceSchedulerInitiated"`, start a replacement on the other host, return the service to `runningCount: 1`, and leave the drained host `DRAINING` with no running tasks.
  - `binpack(memory)` with desiredCount=2 must place both tasks on ONE `containerInstanceArn` while `spread(instanceId)` with desiredCount=2 must use BOTH.

## Steps to Reproduce
1. `ecs create-cluster --cluster-name <c>`.
2. Create an ECS instance role + instance profile carrying `AmazonEC2ContainerServiceforEC2Role`.
3. `ssm get-parameter --name /aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id`.
4. `ec2 run-instances --image-id <ami> --instance-type t3.micro --iam-instance-profile Arn=<profile> --subnet-id <subnet> --security-group-ids <sg> --user-data $(base64 <<< 'echo ECS_CLUSTER=<c> >> /etc/ecs/ecs.config')`.
5. Poll `ecs list-container-instances --cluster <c>`: on the emulator it stays empty for the whole budget. On AWS it returns one ARN, and `ecs describe-container-instances --cluster <c> --container-instances <ci>` reports `ec2InstanceId` = the id from step 4, `status: ACTIVE`, `agentConnected: true`.
6. Repeat step 4 for a second instance to build the two-host estate the `deep-ecs-2-ec2place-*` bodies need; the same poll never converges.
7. With a registered instance in hand, the gated behaviors to verify after the fix:
   a. `ecs put-attributes --cluster <c> --attributes <ten attrs on targetId=<ci>>` (200), then one more attribute -> 400 `AttributeLimitExceededException` / `Attribute per target limit exceeded.`, attribute not applied; delete one attribute and re-issue the identical call -> 200.
   b. `ecs update-container-instances-state --cluster <c> --container-instances <ci> --status DRAINING` -> 200/DRAINING; same call with an unregistered id -> refused, real instance unchanged; then `ecs deregister-container-instance` -> unlisted but described `INACTIVE`.
   c. `ecs discover-poll-endpoint --cluster <c> --container-instance <ci>` -> endpoint; with no registered instance -> refused.
   d. `ecs start-task --cluster <c> --task-definition <td> --container-instances <good-ci> <bad-ci>` -> `tasks[]` for the good one and a per-entry `failures[]` for the bad one.
   e. `ecs run-task --cluster <c> --task-definition <td> --count 3 --launch-type EC2 --placement-constraints type=distinctInstance` on two hosts -> a `failures[]` entry with reason `DistinctInstance placement constraint unsatisfied.`
   f. Two services on two hosts, desiredCount=2 each, one with `--placement-strategy type=binpack,field=memory` and one with `type=spread,field=instanceId` -> binpack on one host, spread across both.
   g. `ecs update-container-instances-state --status DRAINING` on the host running a service task -> task stopped with `stopCode: ServiceSchedulerInitiated`, replacement started on the other host, `runningCount` back to 1.
   h. Drive a task to STOPPED on the instance, then `ecs submit-task-state-change --cluster <c> --task <t> --status RUNNING` -> `lastStatus` must not regress.
Replay: ecs.concurrent-container-instance-state-update, ecs.container-instance-register-state-deregister, ecs.container-state-change-sequencing, ecs.deep-ecs-2-ec2place-binpack-colocates-while-spread-separates, ecs.deep-ecs-2-ec2place-bridge-dynamic-host-port-serves-sibling-and-fixed-port-collision-rejected, ecs.deep-ecs-2-ec2place-distinct-instance-third-task-on-two-hosts-rejected, ecs.deep-ecs-2-ec2place-draining-instance-relocates-service-task-to-other-host, ecs.placement-constraint-memberof-attribute-filters-candidate-instances, ecs.poll-endpoint-requires-registered-instance, ecs.put-attributes-exceeding-limit-rejected, ecs.start-task-batch-partial-failure, ecs.task-state-change-ordering-enforced, ecs.update-container-instances-state-unknown.
