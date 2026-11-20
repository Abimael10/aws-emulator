Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecracc-fargate-ecr-image-execution-role-vs-task-role
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecracc-fargate-ecr-image-execution-role-vs-task-role ecs-fargate-awsvpc-task-definition-rendered-defaults ecs-fargate-task-definition-round-trips-awslogs-and-omits-entry-point ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete ecs-register-task-definition-healthy-dependency-without-health-check-rejected
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|RegisterTaskDefinition|2xx->2xx|$.taskDefinition.compatibilities[*]
Issue-Key: ecs|DescribeTaskDefinition|2xx->2xx|$.taskDefinition.compatibilities[*]
Issue-Key: ecs|RegisterTaskDefinition|2xx->2xx|$.taskDefinition.registeredAt
Issue-Key: ecs|RegisterTaskDefinition|2xx->2xx|$.taskDefinition.volumes[*].host
Issue-Key: ecs|DescribeTaskDefinition|2xx->2xx|$.taskDefinition.registeredAt
Issue-Key: ecs|DescribeTaskDefinition|2xx->2xx|$.taskDefinition.volumes[*].host
Issue-Key: ecs|DeregisterTaskDefinition|2xx->2xx|$.taskDefinition.registeredAt
Issue-Key: ecs|DeregisterTaskDefinition|2xx->2xx|$.taskDefinition.volumes[*].host
Issue-Key: ecs|DescribeTaskDefinition|2xx->2xx|$.taskDefinition.deregisteredAt
Issue-Key: ecs|DeleteTaskDefinitions|2xx->2xx|$.taskDefinitions[*].deregisteredAt
Issue-Key: ecs|DeleteTaskDefinitions|2xx->2xx|$.taskDefinitions[*].registeredAt
Issue-Key: ecs|DeleteTaskDefinitions|2xx->2xx|$.taskDefinitions[*].volumes[*].host
Issue-Key: ecs|ListTaskDefinitions|2xx->2xx|$.taskDefinitionArns[*]
Issue-Key: ecs|RegisterTaskDefinition|2xx->2xx|$.taskDefinition.requiresAttributes[*]
Issue-Key: ecs|RegisterTaskDefinition|2xx->2xx|$.taskDefinition.revision
Issue-Key: ecs|RegisterTaskDefinition|2xx->2xx|$.taskDefinition.taskDefinitionArn
Issue-Commit: 16caeccf34fd30fafc2febabf9312168f85b2a49

# ECS task definition records render wrong compatibilities, volumes and requiresAttributes and accept a definition AWS refuses

> _Note: this issue was identified against the code base at commit `16caeccf34fd30fafc2febabf9312168f85b2a49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTaskDefinition

## Description
Every ECS operation that returns a TaskDefinition structure (RegisterTaskDefinition, DescribeTaskDefinition, DeregisterTaskDefinition, DeleteTaskDefinitions, ListTaskDefinitions) renders that structure from the emulator's own abbreviated model instead of the record AWS renders: the derived `compatibilities` list, the `volumes[].host` default, the derived `requiresAttributes` list and the timestamp members all diverge, and one task definition AWS refuses is registered anyway. 10 causes.

Cause 1 of 10: RegisterTaskDefinition omits MANAGED_INSTANCES from the derived `compatibilities` list.
AWS: Smithy: TaskDefinition$compatibilities targets CompatibilityList of `Compatibility`, whose value set is [EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES]. AWS derives the list from what the definition can actually run on, so a Fargate-capable awsvpc definition is reported as capable of FARGATE and MANAGED_INSTANCES as well as EC2 where applicable; the recorded response carries MANAGED_INSTANCES at compatibilities[1].
Emulator: returns only the pre-MANAGED_INSTANCES set, so compatibilities[1] is 'FARGATE' where AWS reports 'MANAGED_INSTANCES' and the MANAGED_INSTANCES entry never appears.
Why: a client that selects a launch type or a capacity provider by inspecting `compatibilities` concludes the definition cannot run on ECS Managed Instances and refuses to deploy it, or picks a different launch type than AWS would allow.
Fix: the compatibilities-derivation step of the RegisterTaskDefinition handler (the task-definition rendering shared by the read paths), not the serializer.
Traces: ecs-ecracc-fargate-ecr-image-execution-role-vs-task-role#task#6.

Cause 2 of 10: DescribeTaskDefinition repeats the same truncated `compatibilities` list on the read path.
AWS: same rule as cause 1 — DescribeTaskDefinition returns the stored derived list, including MANAGED_INSTANCES.
Emulator: compatibilities[1] is 'FARGATE' where AWS reports 'MANAGED_INSTANCES'.
Why: the gap is not confined to the create response; a client that re-reads the definition still sees the wrong capability set.
Fix: the task-definition read rendering used by DescribeTaskDefinition.
Traces: ecs-ecracc-fargate-ecr-image-execution-role-vs-task-role#task#7.

Cause 3 of 10: for a Fargate awsvpc definition with awslogs, DescribeTaskDefinition emits a `compatibilities` list one element SHORT — the MANAGED_INSTANCES element is absent entirely rather than displaced.
AWS: compatibilities[2] is 'MANAGED_INSTANCES'.
Emulator: compatibilities has no index 2 at all (missing, got None).
Why: same client consequence as cause 1, and it shows the list length — not just its ordering — is wrong, so a fix that only re-orders the list will not close it.
Fix: the compatibilities-derivation step.
Traces: ecs-fargate-task-definition-round-trips-awslogs-and-omits-entry-point#task#4.

Cause 4 of 10: RegisterTaskDefinition drops the empty `host` object of a bind-mount volume, and emits its registeredAt timestamp at sub-millisecond precision.
AWS: Smithy: TaskDefinition$volumes -> VolumeList -> Volume$host targets HostVolumeProperties, whose only member `sourcePath` is optional; the Volume doc states "If an empty volume configuration is specified, by default Amazon ECS uses a host volume" and "If the `host` parameter is empty, then the Docker daemon assigns a host path". AWS therefore echoes a volume declared as a bind mount with `host` PRESENT as an empty object `{}`, not omitted. TaskDefinition$registeredAt is a Timestamp; in this corpus AWS serializes ECS epoch-second timestamps with at most millisecond precision (e.g. 1791557500.895).
Emulator: `volumes[0].host` is absent from the response, and registeredAt is emitted as a 7-decimal float (e.g. 1795135560.890722), which fails the fixture's shape assertion on that member. Hypothesis: the state model stores `host` as an Option that is None for an empty object, and the timestamp is serialized from a raw f64 clock value without rounding to milliseconds. The divergence is the member's presence and the value's precision, not the instant it names.
Why: a client that round-trips a described definition back into RegisterTaskDefinition loses the bind-mount declaration (the volume changes meaning from host volume to an unconfigured volume), and a client that parses the timestamp with a millisecond-precision reader sees a value AWS would never emit.
Fix: the task-definition serializer (volume rendering and the Timestamp serializer).
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#1.

Cause 5 of 10: DescribeTaskDefinition drops the same `volumes[0].host` empty object and repeats the timestamp precision divergence.
AWS: as cause 4.
Emulator: `volumes[0].host` missing (expected {}), registeredAt sub-millisecond.
Why: the read path confirms the omission is stored, not just a create-response artefact.
Fix: the task-definition read rendering.
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#2.

Cause 6 of 10: DeregisterTaskDefinition returns the same defective TaskDefinition body.
AWS: as cause 4; the deregister response echoes the full definition.
Emulator: `volumes[0].host` missing, registeredAt sub-millisecond.
Fix: the deregister handler's response rendering (shared task-definition serializer).
Why: the deregister response is the last view some clients keep of the definition.
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#11.

Cause 7 of 10: after deregistration DescribeTaskDefinition still drops `volumes[0].host` and now emits BOTH deregisteredAt and registeredAt at sub-millisecond precision.
AWS: as cause 4; TaskDefinition$deregisteredAt is a Timestamp serialized the same way.
Emulator: deregisteredAt and registeredAt are 7-decimal floats; `volumes[0].host` missing.
Fix: the task-definition serializer's Timestamp handling, applied to every timestamp member.
Why: a client cannot distinguish AWS-shaped from emulator-shaped timestamps on the INACTIVE read path either.
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#12.

Cause 8 of 10: DeleteTaskDefinitions returns the same defective body in `taskDefinitions[]`.
AWS: as cause 4/7.
Emulator: taskDefinitions[0].volumes[0].host missing, taskDefinitions[0].registeredAt and deregisteredAt sub-millisecond.
Fix: the DeleteTaskDefinitions handler's response rendering.
Why: the delete response is the audit record of what was deleted.
Traces: ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#14.

Cause 9 of 10: a task definition whose container declares `dependsOn` condition HEALTHY on a container with NO healthCheck is registered instead of refused, so ListTaskDefinitions lists an ARN that must not exist.
AWS: recorded behavior only — AWS refuses the registration (the fixture's ListTaskDefinitions response contains NO task definition ARN for the family at this step). Smithy: RegisterTaskDefinition models ClientException and InvalidParameterException for request-level refusals; the model does not state the dependsOn/healthCheck precondition in a trait, it is enforced by the service.
Emulator: ListTaskDefinitions returns an extra ARN `arn:aws:ecs:<region>:<account>:task-definition/<family>:1`, i.e. the refused registration was persisted as revision 1.
Why: a client that uses a registration failure as its signal that the dependency graph is invalid gets a usable task definition ARN instead, and every later revision number is shifted by one.
Fix: the RegisterTaskDefinition handler's container-dependency validation (reject a HEALTHY dependsOn condition on a container without a healthCheck) — the state must not record the definition.
Traces: ecs-register-task-definition-healthy-dependency-without-health-check-rejected#task#2.

Cause 10 of 10: the subsequent valid RegisterTaskDefinition returns revision 2 instead of 1 and a `requiresAttributes` list the emulator invents rather than derives.
AWS: Smithy: TaskDefinition$revision — "When you register a task definition for the first time, the revision is 1"; the response ARN therefore ends `:1`. TaskDefinition$requiresAttributes is derived from the FEATURES the definition uses: the recording carries {name: 'ecs.capability.container-health-check'}, {name: 'ecs.capability.container-ordering'} and {name: 'com.amazonaws.ecs.capability.docker-remote-api.1.29'} for a definition using a container health check plus container ordering.
Emulator: revision 2 and ARN `...:2` (a consequence of cause 9 persisting the refused revision 1), and requiresAttributes is a fixed list headed by {name: 'com.amazonaws.ecs.capability.docker-remote-api.1.18'} and {name: 'ecs.capability.task-eni'} with no container-health-check, container-ordering or docker-remote-api.1.29 entry; `compatibilities[2]` ('MANAGED_INSTANCES') is missing as in cause 3, and registeredAt is sub-millisecond as in cause 4.
Why: requiresAttributes is what the scheduler matches against container-instance attributes, so a client (or the emulator's own placement) decides a definition can run on an instance that lacks container-ordering/health-check support, and the revision/ARN mismatch breaks any client that pinned `family:1`.
Fix: the RegisterTaskDefinition handler's requiresAttributes derivation and revision allocation.
Traces: ecs-register-task-definition-healthy-dependency-without-health-check-rejected#task#3.

## Evidence
Cause 1 — ecs-ecracc-fargate-ecr-image-execution-role-vs-task-role#task#6, RegisterTaskDefinition, 200 vs 200: `$.taskDefinition.compatibilities[1]` expected 'MANAGED_INSTANCES', got 'FARGATE'.
Cause 2 — same fixture #task#7, DescribeTaskDefinition: `$.taskDefinition.compatibilities[1]` expected 'MANAGED_INSTANCES', got 'FARGATE'.
Cause 3 — ecs-fargate-task-definition-round-trips-awslogs-and-omits-entry-point#task#4, DescribeTaskDefinition: `$.taskDefinition.compatibilities[2]` expected 'MANAGED_INSTANCES', got None (list is short).
Cause 4 — ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete#task#1, RegisterTaskDefinition: `$.taskDefinition.volumes[0].host` expected {}, got None; `$.taskDefinition.registeredAt` fails its shape assertion, got 1795135560.890722 (AWS emits at most millisecond precision, cf. 1791557500.895 elsewhere in this corpus).
Cause 5 — same fixture #task#2, DescribeTaskDefinition: identical two keys.
Cause 6 — same fixture #task#11, DeregisterTaskDefinition: identical two keys.
Cause 7 — same fixture #task#12, DescribeTaskDefinition: `$.taskDefinition.deregisteredAt` (got 1795135620.9791718) and `$.taskDefinition.registeredAt` fail their shape assertions; `volumes[0].host` missing.
Cause 8 — same fixture #task#14, DeleteTaskDefinitions: `$.taskDefinitions[0].deregisteredAt`, `$.taskDefinitions[0].registeredAt`, `$.taskDefinitions[0].volumes[0].host` — same three.
Cause 9 — ecs-register-task-definition-healthy-dependency-without-health-check-rejected#task#2, ListTaskDefinitions, 200 vs 200: `$.taskDefinitionArns[0]` expected None, got 'arn:aws:ecs:us-west-2:000000000000:task-definition/<family>:1' (the refused registration was persisted).
Cause 10 — same fixture #task#3, RegisterTaskDefinition: `$.taskDefinition.revision` expected 1, got 2; `$.taskDefinition.taskDefinitionArn` expected '...:1', got '...:2'; `$.taskDefinition.requiresAttributes[0]` expected {'name': 'ecs.capability.container-health-check'}, got {'name': 'com.amazonaws.ecs.capability.docker-remote-api.1.18'}; `requiresAttributes[1]` expected {'name': 'ecs.capability.container-ordering'}, got {'name': 'ecs.capability.task-eni'}; `requiresAttributes[4]` expected {'name': 'com.amazonaws.ecs.capability.docker-remote-api.1.29'}, got None; `compatibilities[2]` expected 'MANAGED_INSTANCES', got None.
Note: the ARNs, family names, revisions and instants above are the recorded run's values filling the templates — the requirement is the member's presence, the derived list contents and the ARN's `:<revision>` suffix, not those literals.

## Steps to Reproduce
1. Replay fixture ecs-ecracc-fargate-ecr-image-execution-role-vs-task-role against the emulator and compare step task#6 (RegisterTaskDefinition) and task#7 (DescribeTaskDefinition): `taskDefinition.compatibilities` omits MANAGED_INSTANCES.
2. Replay ecs-fargate-task-definition-round-trips-awslogs-and-omits-entry-point step task#4 — compatibilities has only two entries.
3. Replay ecs-task-definition-bridge-network-explicit-volume-tag-change-revision-deregister-and-delete and compare steps task#1, task#2, task#11, task#12, task#14 — every TaskDefinition body omits `volumes[0].host` ({} in AWS) and emits registeredAt/deregisteredAt with sub-millisecond precision.
4. Replay ecs-register-task-definition-healthy-dependency-without-health-check-rejected: at task#1 register a definition whose container has `dependsOn: [{containerName: X, condition: HEALTHY}]` where X has no healthCheck — the emulator returns 200 instead of refusing; at task#2 ListTaskDefinitions then lists `<family>:1`, and at task#3 the valid registration comes back as revision 2 with a fixed requiresAttributes list.
