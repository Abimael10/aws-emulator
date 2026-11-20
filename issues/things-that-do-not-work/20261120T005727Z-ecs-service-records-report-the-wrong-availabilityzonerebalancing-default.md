Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ntraf-service-binding-accepted-once-a-listener-forwards
Issue-Step: task#18
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ntraf-service-binding-accepted-once-a-listener-forwards ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|CreateService|2xx->2xx|$.service.taskSets[*].serviceArn
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*].taskSets[*].serviceArn
Issue-Key: ecs|CreateService|2xx->2xx|$.service.createdAt
Issue-Key: ecs|CreateService|2xx->2xx|$.service.deployments[*].createdAt
Issue-Key: ecs|CreateService|2xx->2xx|$.service.deployments[*].updatedAt
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*].createdAt
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*].deployments[*].createdAt
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*].deployments[*].updatedAt
Issue-Key: ecs|UpdateService|2xx->2xx|$.service.createdAt
Issue-Key: ecs|UpdateService|2xx->2xx|$.service.deployments[*].createdAt
Issue-Key: ecs|UpdateService|2xx->2xx|$.service.deployments[*].updatedAt
Issue-Key: ecs|DeleteService|2xx->2xx|$.service.createdAt
Issue-Key: ecs|DeleteService|2xx->2xx|$.service.deployments[*].createdAt
Issue-Key: ecs|DeleteService|2xx->2xx|$.service.deployments[*].updatedAt
Issue-Related: 20261103T150504Z-ecs-service-responses-drop-deployment-configuration-task-sets.md
Issue-Related: 20261116T130854Z-ecs-service-records-drop-configuration-the-emulator-was.md
Issue-Commit: 16caeccf34fd30fafc2febabf9312168f85b2a49

# ECS service records report the wrong availabilityZoneRebalancing default, an unresolved platformVersion and a malformed task set serviceArn

> _Note: this issue was identified against the code base at commit `16caeccf34fd30fafc2febabf9312168f85b2a49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Every ECS operation that returns a Service structure (CreateService, DescribeServices, UpdateService, DeleteService) renders members the emulator never resolved: `availabilityZoneRebalancing` is reported DISABLED where AWS defaults it to ENABLED, `platformVersion` is echoed as the literal 'LATEST' instead of the resolved Fargate platform version, a task set's `serviceArn` omits the cluster segment, and the Service/Deployment timestamp members are serialized at sub-millisecond precision. 7 causes. Related but NOT covering: open issue 20261116T130854Z-ecs-service-records-drop-configuration-the-emulator-was.md describes the same family of dropped service configuration but carries none of the replay keys below, so these clusters are unowned and re-replay is the only way to confirm a fix.

Cause 1 of 7: CreateService returns platformVersion 'LATEST' and a task set serviceArn with no cluster segment.
AWS: Smithy: Service$platformVersion — "The platform version to run your service on ... If one isn't specified, the LATEST platform version is used"; AWS resolves LATEST to the concrete version it ran the service on and reports that (the recording carries '1.4.0'). Service$taskSets -> TaskSet$serviceArn is the ARN of the owning service, and an ECS service ARN in the long-ARN format is `arn:aws:ecs:<region>:<account>:service/<clusterName>/<serviceName>` — the cluster name is part of the path (the recording's value carries both segments).
Emulator: platformVersion is the un-resolved literal 'LATEST', and taskSets[0].serviceArn is `arn:aws:ecs:<region>:<account>:service/<serviceName>` — the `<clusterName>/` segment is missing.
Why: a client that pins or compares platform versions cannot tell which Fargate platform the service runs; and a malformed task set serviceArn cannot be parsed back into (cluster, service) or passed to any operation that addresses a service by ARN, so blue/green tooling that follows taskSets[].serviceArn breaks.
Fix: the CreateService handler's platform-version resolution and the service-ARN construction used when rendering TaskSet (the service/task-set state rendering), not the serializer.
Traces: ecs-ntraf-service-binding-accepted-once-a-listener-forwards#task#18.

Cause 2 of 7: DescribeServices repeats cause 1 and additionally reports availabilityZoneRebalancing DISABLED.
AWS: Smithy: Service$availabilityZoneRebalancing targets AvailabilityZoneRebalancing [ENABLED, DISABLED] and its doc states "For create service requests, when no value is specified for AvailabilityZoneRebalancing, Amazon ECS defaults the value to ENABLED". A service created without the member therefore reads back ENABLED.
Emulator: `services[1].availabilityZoneRebalancing` is 'DISABLED', `platformVersion` 'LATEST', `taskSets[0].serviceArn` missing the cluster segment.
Why: a client that reads the service to decide whether to turn AZ rebalancing on issues a redundant UpdateService, or concludes rebalancing is off when AWS has it on.
Fix: the service state's create-time default for availabilityZoneRebalancing, plus the rendering named in cause 1.
Traces: ecs-ntraf-service-binding-accepted-once-a-listener-forwards#task#21.

Cause 3 of 7: CreateService emits Service$createdAt and Deployment createdAt/updatedAt at sub-millisecond precision.
AWS: Smithy: Service$createdAt is a Timestamp ("The Unix timestamp for the time when the service was created"); in this corpus AWS serializes ECS epoch-second timestamps with at most millisecond precision (e.g. 1791557500.895).
Emulator: emits 7-decimal floats (e.g. 1795135306.0857944), failing the fixture's shape assertion on each of those members. Hypothesis: the timestamp is serialized straight from an f64 clock reading with no rounding to milliseconds. The divergence is the precision/shape, not the instant named.
Why: a client whose timestamp reader is written for AWS's millisecond-precision encoding reads a value AWS would never emit.
Fix: the Timestamp serializer used by the service rendering.
Traces: ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive#task#3.

Cause 4 of 7: DescribeServices repeats the sub-millisecond timestamp shape on the read path.
AWS: as cause 3.
Emulator: services[0].createdAt and services[0].deployments[0].createdAt/updatedAt are 7-decimal floats.
Why: the divergence is in stored/rendered state, not only the create response.
Fix: as cause 3.
Traces: ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive#task#4.

Cause 5 of 7: UpdateService reports availabilityZoneRebalancing DISABLED for a service created without the member, plus the timestamp shape.
AWS: as cause 2 — the service was defaulted ENABLED at create, and Smithy states an update with no value preserves the existing value, so the update response still reports ENABLED.
Emulator: `service.availabilityZoneRebalancing` 'DISABLED'; createdAt and deployments[0].createdAt/updatedAt sub-millisecond.
Why: the update response is what a client caches as the service's new state.
Fix: the service state's create-time default and the update path's preserve-existing-value rule.
Traces: ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive#task#11.

Cause 6 of 7: DescribeServices after the update still reports availabilityZoneRebalancing DISABLED, plus the timestamp shape.
AWS: as cause 2/5.
Emulator: `services[0].availabilityZoneRebalancing` 'DISABLED'; the three timestamps sub-millisecond.
Fix: as cause 5.
Why: confirms the wrong default is persisted, not computed per response.
Traces: ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive#task#12.

Cause 7 of 7: DeleteService returns the same defective Service body (availabilityZoneRebalancing DISABLED and sub-millisecond timestamps).
AWS: as cause 2/3 — the delete response echoes the final service record.
Emulator: `service.availabilityZoneRebalancing` 'DISABLED'; createdAt and deployments[0].createdAt/updatedAt sub-millisecond.
Why: the delete response is the last record a client keeps of the service.
Fix: the DeleteService handler's response rendering (shared service serializer).
Traces: ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive#task#13.

## Evidence
Cause 1 — ecs-ntraf-service-binding-accepted-once-a-listener-forwards#task#18, CreateService, 200 vs 200: `$.service.platformVersion` expected '1.4.0', got 'LATEST'; `$.service.taskSets[0].serviceArn` expected 'arn:aws:ecs:us-west-2:000000000000:service/<cluster>/<service>', got 'arn:aws:ecs:us-west-2:000000000000:service/<service>' (cluster segment absent).
Cause 2 — same fixture #task#21, DescribeServices: `$.services[1].availabilityZoneRebalancing` expected 'ENABLED', got 'DISABLED'; `$.services[1].platformVersion` expected '1.4.0', got 'LATEST'; `$.services[1].taskSets[0].serviceArn` same malformed ARN.
Cause 3 — ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive#task#3, CreateService: `$.service.createdAt`, `$.service.deployments[0].createdAt`, `$.service.deployments[0].updatedAt` all fail their shape assertion, got 1795135306.0857944.
Cause 4 — same fixture #task#4, DescribeServices: `$.services[0].createdAt`, `$.services[0].deployments[0].createdAt`, `$.services[0].deployments[0].updatedAt` — same (the cluster also covers task#6, task#8, task#10).
Cause 5 — same fixture #task#11, UpdateService: `$.service.availabilityZoneRebalancing` expected 'ENABLED', got 'DISABLED'; the three timestamps as above.
Cause 6 — same fixture #task#12, DescribeServices: `$.services[0].availabilityZoneRebalancing` expected 'ENABLED', got 'DISABLED' (cluster also covers task#14).
Cause 7 — same fixture #task#13, DeleteService: `$.service.availabilityZoneRebalancing` expected 'ENABLED', got 'DISABLED'.
The region, account, cluster/service names and instants above are the recorded run's values filling the templates; the requirements are the ENABLED create-time default, a resolved concrete platformVersion, the `service/<cluster>/<service>` ARN shape and AWS's millisecond timestamp encoding.

## Steps to Reproduce
1. Replay fixture ecs-ntraf-service-binding-accepted-once-a-listener-forwards and compare step task#18 (CreateService) and task#21 (DescribeServices): platformVersion comes back 'LATEST' instead of the resolved Fargate version, taskSets[0].serviceArn omits the cluster segment, and availabilityZoneRebalancing is DISABLED.
2. Replay ecs-service-zero-desired-explicit-deployment-defaults-and-empty-lists-tag-change-then-inactive: create a service WITHOUT availabilityZoneRebalancing (task#3), then read it (task#4), update it without the member (task#11), re-read it (task#12) and delete it (task#13) — every response reports DISABLED instead of ENABLED and serializes createdAt/updatedAt with 7 decimal places.
