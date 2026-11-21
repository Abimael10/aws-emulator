Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-service-lifecycle-on-cluster
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-service-lifecycle-on-cluster ecs-ntraf-service-binding-accepted-once-a-listener-forwards
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|UpdateService|2xx->2xx|$.service.availabilityZoneRebalancing
Issue-Key: ecs|DeleteService|2xx->2xx|$.service.availabilityZoneRebalancing
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*].availabilityZoneRebalancing
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*]
Issue-Related: 20261103T150504Z-ecs-service-responses-drop-deployment-configuration-task-sets.md
Issue-Commit: 1d4ff0ec19313d445ee1c97e91766984f5ed6bfa

# ECS service records drop configuration the emulator was given: availabilityZoneRebalancing, resolved platformVersion and taskSets

> _Note: this issue was identified against the code base at commit `1d4ff0ec19313d445ee1c97e91766984f5ed6bfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
Five net-new causes in the ECS service record the emulator stores and serves back. Every read of a service (UpdateService, DeleteService, DescribeServices) returns a Service whose configuration does not match what the service was created/updated with, so a client cannot read back its own configuration.

Cause 1 of 5: UpdateService returns availabilityZoneRebalancing DISABLED for a service whose stored value is ENABLED.
AWS: Smithy ECS UpdateService -> output member service: Service; Service.availabilityZoneRebalancing targets the AvailabilityZoneRebalancing enum (values ENABLED, DISABLED). The member's documentation states the default explicitly: "For create service requests, when no value is specified for AvailabilityZoneRebalancing, Amazon ECS defaults the value to ENABLED", and "For update service requests, when no value is specified ... Amazon ECS defaults to the existing service's AvailabilityZoneRebalancing value". So a service created without the field, and then updated without the field, must report ENABLED.
Emulator: UpdateService answers 200 with $.service.availabilityZoneRebalancing = 'DISABLED' where the recording has 'ENABLED'.
Why: a client that reads back availabilityZoneRebalancing to decide whether AZ rebalancing is on (or that asserts its own CreateService defaults) sees the feature off on a service AWS reports as on, and an UpdateService that omits the field appears to have turned the feature off.
Fix: the service state model's default for availabilityZoneRebalancing (default ENABLED on create, inherit-existing on update) and the Service serializer shared by CreateService/UpdateService/DescribeServices/DeleteService.
Traces: ecs-service-lifecycle-on-cluster#task#5 (UpdateService, 200 vs 200, $.service.availabilityZoneRebalancing).

Cause 2 of 5: DeleteService returns the same wrong value on the deleted service record.
AWS: Smithy ECS DeleteService -> output member service: Service, same Service.availabilityZoneRebalancing member and default rule as cause 1; deletion does not change the stored value.
Emulator: DeleteService answers 200 with $.service.availabilityZoneRebalancing = 'DISABLED' where the recording has 'ENABLED'.
Why: the last observable copy of the service a client gets also misreports its configuration.
Fix: same Service serializer/state default as cause 1.
Traces: ecs-service-lifecycle-on-cluster#task#11 (DeleteService).

Cause 3 of 5: DescribeServices returns the same wrong value.
AWS: Smithy ECS DescribeServices -> output member services: Services (list of Service); same member and default rule as cause 1.
Emulator: DescribeServices answers 200 with $.services[0].availabilityZoneRebalancing = 'DISABLED' where the recording has 'ENABLED'.
Why: the primary read path for a service misreports its configuration, so reconcile loops and CDK/terraform-style drift detection see permanent drift.
Fix: same as cause 1.
Traces: ecs-service-lifecycle-on-cluster#task#12 (DescribeServices).

Cause 4 of 5: DescribeServices on a Fargate service with a blue/green task set reports an unresolved platformVersion, omits taskSets entirely, and again reports availabilityZoneRebalancing DISABLED.
AWS: Smithy ECS Service members platformVersion (String) and taskSets (list of TaskSet). The model documents platformVersion only as "The platform version to run your service on ... If one isn't specified, the LATEST platform version is used" — recorded behavior only for the resolution: the recording shows the stored service reporting the concrete Fargate platform version it resolved to ('1.4.0'), not the literal 'LATEST' token, and reporting the PRIMARY task set created for it under taskSets (with its taskSetArn, loadBalancers, networkConfiguration, scale, status PRIMARY and counts).
Emulator: DescribeServices answers 200 with $.services[1].platformVersion = 'LATEST' (expected the resolved concrete version), $.services[1].taskSets[0] missing (expected the PRIMARY task set object), and $.services[1].availabilityZoneRebalancing = 'DISABLED' (expected 'ENABLED').
Why: a client doing a blue/green or external deployment reads taskSets from DescribeServices to find the revision it must shift traffic to; with the list absent the deployment cannot be driven at all, and an unresolved 'LATEST' makes a pinned-platform assertion fail.
Fix: the service state's task-set association and platform-version resolution, plus the Service serializer's taskSets/platformVersion members.
Traces: ecs-ntraf-service-binding-accepted-once-a-listener-forwards#task#21 (DescribeServices).

Cause 5 of 5: DescribeServices after a service create/update cycle returns a service object that does not match the recorded service record as a whole.
AWS: Smithy ECS DescribeServices -> services: Services. The recorded service record carries, for an ECS-controller service, availabilityZoneRebalancing, createdBy, currentServiceDeployment, currentServiceRevisions (arn + requested/pending/running task counts), the full deploymentConfiguration (including deploymentCircuitBreaker with resetOnHealthyTask and thresholdConfiguration) and the service event list; recorded behavior only for which of those the emulator may omit.
Emulator: DescribeServices answers 200 but the whole $.services[0] object compares unequal to the recorded one (reported as `$.services[0]: missing`), i.e. the served service record is not the record AWS serves for the same create/update sequence.
Why: any client that round-trips a service description — to assert its deployment configuration, to find the current service deployment ARN, or to read service events — reads a record with different members than AWS returns.
Fix: the DescribeServices handler and the Service serializer / service state that populates those members.
Traces: ecs-service-lifecycle-on-cluster#task#6 (DescribeServices).

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 (score 91.6%, emulator up).

Cause 1 — ecs-service-lifecycle-on-cluster#task#5, UpdateService, expected 200 / actual 200: $.service.availabilityZoneRebalancing: value_mismatch (expected 'ENABLED', got 'DISABLED').
Cause 2 — ecs-service-lifecycle-on-cluster#task#11, DeleteService, 200/200: $.service.availabilityZoneRebalancing: value_mismatch (expected 'ENABLED', got 'DISABLED').
Cause 3 — ecs-service-lifecycle-on-cluster#task#12, DescribeServices, 200/200: $.services[0].availabilityZoneRebalancing: value_mismatch (expected 'ENABLED', got 'DISABLED').
Cause 4 — ecs-ntraf-service-binding-accepted-once-a-listener-forwards#task#21, DescribeServices, 200/200: $.services[1].availabilityZoneRebalancing: value_mismatch (expected 'ENABLED', got 'DISABLED'); $.services[1].platformVersion: value_mismatch (expected '1.4.0', got 'LATEST'); $.services[1].taskSets[0]: missing (expected the PRIMARY task set object carrying taskSetArn, status 'PRIMARY', launchType FARGATE, loadBalancers[0].targetGroupArn, awsvpcConfiguration, scale 100 PERCENT; got None).
Cause 5 — ecs-service-lifecycle-on-cluster#task#6, DescribeServices, 200/200: $.services[0]: missing — the recorded object carries availabilityZoneRebalancing 'ENABLED', createdBy, currentServiceDeployment, currentServiceRevisions[0] with task counts, deploymentConfiguration.deploymentCircuitBreaker.thresholdConfiguration {type BOUNDED_PERCENT, value 50}, deploymentConfiguration.bakeTimeInMinutes, and the steady-state service events.

The concrete ARNs, cluster/service names, ids and timestamps above are one run's values and are not part of the requirement; the diverging members, the enum value and the presence of taskSets are.

## Steps to Reproduce
1. Replay fixture ecs-service-lifecycle-on-cluster against the emulator and stop at step task#5: CreateService on a cluster without passing availabilityZoneRebalancing, then UpdateService (e.g. desiredCount) and read $.service.availabilityZoneRebalancing — the emulator answers 'DISABLED', AWS answers 'ENABLED'.
2. Continue the same fixture to task#6 (DescribeServices), task#11 (DeleteService) and task#12 (DescribeServices) to see the same member diverge and the whole service record diverge at task#6.
3. Replay fixture ecs-ntraf-service-binding-accepted-once-a-listener-forwards to step task#21: create a FARGATE service with a blue/green task set behind an ALB target group, then DescribeServices — $.services[1].taskSets is absent and $.services[1].platformVersion is the literal 'LATEST'.
