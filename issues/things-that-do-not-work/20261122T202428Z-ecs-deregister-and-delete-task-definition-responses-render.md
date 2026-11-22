Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-container-instance-and-attribute-errors
Issue-Step: teardown#13
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-container-instance-and-attribute-errors ecs-run-task-and-task-protection ecs-service-lifecycle-on-cluster ecs-start-task-error-paths ecs-task-definition-lifecycle ecs-task-definition-list-sort-and-status-partition ecs-task-definition-revision-lifecycle ecs-task-set-external-deployment-workflow ecs-taskdef-tagging-lifecycle ecs-taskset-basic-lifecycle ecs-taskset-errors ecs-taskset-lifecycle-ec2 ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DeregisterTaskDefinition|2xx->2xx|$.taskDefinition.compatibilities[*]
Issue-Key: ecs|DeleteTaskDefinitions|2xx->2xx|$.taskDefinitions[*].compatibilities[*]
Issue-Related: 20261120T005727Z-ecs-task-definition-records-render-wrong-compatibilities-volumes.md
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# ECS deregister and delete task-definition responses render a MANAGED_INSTANCES compatibility and drop the empty volume host

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterTaskDefinition

## Description
The emulator's TaskDefinition serializer injects a third `compatibilities` entry, `MANAGED_INSTANCES`, into every task-definition record, and it omits the `volumes[].host` structure for a bind-mount volume that was registered with an empty `host`. The open issue 20261120T233305Z-ecs-describetaskdefinition-registertaskdefinition-over-report-compatibilities-managed-instances.md and 20261120T005727Z-ecs-task-definition-records-render-wrong-compatibilities-volumes.md cover the Register/Describe read paths; the four causes below are the SAME rendering fault on the deregister and delete paths, which no open issue carries a replay key for. 4 causes.

Cause 1 of 4: DeregisterTaskDefinition over-reports compatibilities.
AWS: Smithy: DeregisterTaskDefinition output member `taskDefinition` -> TaskDefinition; TaskDefinition.compatibilities is a list of the `Compatibility` enum whose value set is [EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES]. AWS derives the list from what the registered revision can actually run on, so a revision registered with requiresCompatibilities [EC2, FARGATE] and no managed-instance configuration comes back with exactly two entries; the recorded response has no `compatibilities[2]`.
Emulator: returns a third element, `compatibilities[2] = "MANAGED_INSTANCES"`, on every deregistered revision (status 200 both sides).
Why: a client that reads `compatibilities` to decide which launch types a revision supports is told the revision can run on ECS Managed Instances when it cannot, so launch-type selection and capacity-provider validation built on that list pick an unsupported target.
Fix: the compatibilities derivation used by the task-definition serializer (the DeregisterTaskDefinition handler's rendering of the stored revision) must emit only the launch types the revision actually supports instead of appending MANAGED_INSTANCES unconditionally.
Traces: ecs-container-instance-and-attribute-errors#teardown#13 (representative; 14 steps in the cluster, including ecs-task-definition-lifecycle#task#7, ecs-taskset-errors#teardown#14).

Cause 2 of 4: DeregisterTaskDefinition also drops the empty `host` of a bind-mount volume.
AWS: Smithy: Volume.host -> HostVolumeProperties, documented as "specified when you use bind mount host volumes... If the `host` parameter is empty, then the Docker daemon assigns a host path for your data volume", and HostVolumeProperties has one optional member, `sourcePath`. A volume registered as `{"name": ..., "host": {}}` is echoed back with `host` present and empty; the recorded response carries `volumes[0].host = {}`. The same step also shows the Cause 1 extra MANAGED_INSTANCES entry.
Emulator: `volumes[0].host` is absent from the record entirely (status 200 both sides).
Why: a client round-tripping a revision (reading it and re-registering it, or diffing it to detect drift) loses the signal that the volume is a bind-mount host volume rather than a Docker/EFS volume, so the re-registered revision changes volume type.
Fix: the task-definition volume serializer must render `host` as an empty structure when the stored volume has a host with no sourcePath, instead of dropping an all-default nested structure.
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#11.

Cause 3 of 4: DeleteTaskDefinitions renders the same two defects.
AWS: Smithy: DeleteTaskDefinitions output `taskDefinitions` -> TaskDefinitionList, the same TaskDefinition shape, so the deleted revisions carry the derived compatibilities and the echoed `volumes[].host` as in Causes 1 and 2. The recorded response has no `taskDefinitions[0].compatibilities[2]` and has `taskDefinitions[0].volumes[0].host = {}`.
Emulator: returns `taskDefinitions[0].compatibilities[2] = "MANAGED_INSTANCES"` and omits `taskDefinitions[0].volumes[0].host` (status 200 both sides).
Why: the delete response is the last observation a client gets of a revision; a caller archiving or asserting on it records a launch type the revision never had and loses the bind-mount volume type.
Fix: same serializer as Causes 1 and 2, reached through the DeleteTaskDefinitions handler.
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#14.

Cause 4 of 4: DeleteTaskDefinitions over-reports compatibilities on revisions with no volumes.
AWS: as Cause 3 for the compatibilities list; the recorded response has no `taskDefinitions[0].compatibilities[2]`.
Emulator: returns `taskDefinitions[0].compatibilities[2] = "MANAGED_INSTANCES"` (status 200 both sides).
Why: as Cause 1 — the reported launch-type set is wrong for every deleted revision, not only the ones carrying volumes.
Fix: as Cause 1.
Traces: ecs-task-definition-lifecycle#task#8 (representative; 5 steps in the cluster, including ecs-task-definition-revision-lifecycle#task#10 and ecs-taskset-basic-lifecycle#teardown#16).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout.

Cause 1 — ecs-container-instance-and-attribute-errors#teardown#13, DeregisterTaskDefinition, expected 200 / actual 200: "$.taskDefinition.compatibilities[2]: extra (expected None, got 'MANAGED_INSTANCES')". 14 steps in cluster.

Cause 2 — ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#11, DeregisterTaskDefinition, expected 200 / actual 200: "$.taskDefinition.compatibilities[2]: extra (expected None, got 'MANAGED_INSTANCES')" and "$.taskDefinition.volumes[0].host: missing (expected {}, got None)".

Cause 3 — ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#14, DeleteTaskDefinitions, expected 200 / actual 200: "$.taskDefinitions[0].compatibilities[2]: extra (expected None, got 'MANAGED_INSTANCES')" and "$.taskDefinitions[0].volumes[0].host: missing (expected {}, got None)".

Cause 4 — ecs-task-definition-lifecycle#task#8, DeleteTaskDefinitions, expected 200 / actual 200: "$.taskDefinitions[0].compatibilities[2]: extra (expected None, got 'MANAGED_INSTANCES')". 5 steps in cluster.

Model evidence: smithy_query shape=Compatibility -> values [EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES]; shape=Volume member host -> HostVolumeProperties; shape=HostVolumeProperties -> single optional member sourcePath; operation=DeregisterTaskDefinition/DeleteTaskDefinitions -> output TaskDefinition / TaskDefinitionList.

## Steps to Reproduce
1. Replay fixture ecs-container-instance-and-attribute-errors, step teardown#13 (Cause 1): register a task definition with requiresCompatibilities [EC2, FARGATE], then DeregisterTaskDefinition it and read `taskDefinition.compatibilities` — a third MANAGED_INSTANCES entry is present.
2. Replay fixture ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete, steps task#11 and task#14 (Causes 2 and 3): register a bridge-mode revision with volumes [{name, host:{}}], then DeregisterTaskDefinition and DeleteTaskDefinitions it — `volumes[0].host` is absent and MANAGED_INSTANCES is added.
3. Replay fixture ecs-task-definition-lifecycle, step task#8 (Cause 4): DeleteTaskDefinitions on a registered revision — `taskDefinitions[0].compatibilities` carries MANAGED_INSTANCES.
