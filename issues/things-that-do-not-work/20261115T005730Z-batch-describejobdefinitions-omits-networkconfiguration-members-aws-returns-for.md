Issue-Source: trace-sweep-divergence
Issue-Trace: batch-job-definition-preserves-untagged-image
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: batch-job-definition-preserves-untagged-image
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch|DescribeJobDefinitions|2xx->2xx|$.jobDefinitions[*]
Issue-Key: batch|DescribeJobDefinitions|2xx->2xx|$.jobDefinitions[*].containerProperties.networkConfiguration.interfaceConfigurations
Issue-Commit: a34634431053f87e29c3a7a7780a847b65f0b3c0

# batch DescribeJobDefinitions omits NetworkConfiguration members AWS returns for a Fargate job definition

> _Note: this issue was identified against the code base at commit `a34634431053f87e29c3a7a7780a847b65f0b3c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobDefinitions

## Description
DescribeJobDefinitions serializes a registered Fargate container job definition with an incomplete containerProperties.networkConfiguration: the member list AWS emits for that structure is pruned, so a read of a job definition does not round-trip the shape AWS returns. Both causes below are the same read operation on the same registered definition, at two different points of the fixture, and both reduce to the same omission in how the job-definition record is serialized.

Cause 1 of 2: DescribeJobDefinitions right after RegisterJobDefinition returns containerProperties.networkConfiguration without interfaceConfigurations.
AWS: recorded behavior only — the on-disk Smithy model (com.amazonaws.batch#NetworkConfiguration) declares only the member `assignPublicIp` and says nothing about `interfaceConfigurations`, while the recorded 200 response for this step returns containerProperties.networkConfiguration as `{"assignPublicIp": "ENABLED", "interfaceConfigurations": []}` — i.e. the real service reports the interface-configuration list and reports it as an empty list when the registration supplied none. The recorded whole-object comparison for jobDefinitions[0] also shows the deprecated ContainerProperties members `vcpus` and `memory` present in the recorded containerProperties for this Fargate definition (Smithy: ContainerProperties.vcpus / ContainerProperties.memory, both @deprecated "use resourceRequirements instead", and both documented as unsupported for Fargate) where the emulator emits neither key. Status is 200 on both sides; no error code or message is involved in this cause.
Emulator: answers 200 with containerProperties.networkConfiguration = `{"assignPublicIp": "ENABLED"}` — interfaceConfigurations is absent (replay: "$.jobDefinitions[0]: missing ... got {... 'networkConfiguration': {'assignPublicIp': 'ENABLED'} ...}"), and the jobDefinitions[0] object additionally diverges because the recorded object carries `vcpus` and `memory` keys the emulator's object does not have. Every other member of the definition (name, arn, revision, status, type, parameters, image, executionRoleArn, resourceRequirements, fargatePlatformConfiguration, tags, platformCapabilities, containerOrchestrationType) matches.
Why: a client that reads back a job definition to inspect or re-submit its networking — deciding whether to add interface configurations, diffing a registered revision against desired state, or copying containerProperties into a new RegisterJobDefinition call — sees the member as unset rather than as an empty list. Code that iterates networkConfiguration.interfaceConfigurations without a null guard faults, and an idempotent register-if-changed loop computes a spurious difference on every pass and re-registers a revision that did not change.
Fix: the DescribeJobDefinitions output serializer for ContainerProperties/NetworkConfiguration in the batch service crate — it must emit networkConfiguration's full modeled member set, including the interface-configuration list serialized as an empty list when the stored definition has no entries, rather than skipping empty collections. The same pruning of deprecated ContainerProperties members (vcpus/memory) belongs in the same serializer.
Traces: fixture batch-job-definition-preserves-untagged-image, step task#4 (DescribeJobDefinitions, expected 200, got 200 with jobDefinitions[0] mismatched).

Cause 2 of 2: the same omission is still present on the teardown read of the definition, isolated to exactly one path.
AWS: recorded behavior only — the recorded 200 response for the teardown DescribeJobDefinitions carries `$.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations: []`.
Emulator: answers 200 but that path is missing entirely (replay: "$.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations: missing (expected [], got None)"). This step's cluster is keyed on that single path, which confirms the omission is unconditional on the serializer rather than an artifact of the registration call in cause 1.
Why: the gap is not transient and does not heal after the definition has been described once or updated, so no client-side retry or re-read recovers the member; every read of every Fargate job definition is short this member for the lifetime of the definition.
Fix: same component as cause 1 — the batch job-definition read serializer; fixing the empty-collection emission for NetworkConfiguration closes both steps.
Traces: fixture batch-job-definition-preserves-untagged-image, step teardown#6 (DescribeJobDefinitions, expected 200, got 200).

Related but not covering this: open issue 20261104T041055Z-batch-ecr-lambda-logs-a-cfn-created-ecr.md reports DescribeComputeEnvironments/DescribeJobs omitting empty collections, and 20261112T140115Z-batch-a-fargate-job-never-reaches-succeeded-and.md reports DescribeJobs omitting JobDetail empty collections; neither covers DescribeJobDefinitions, and the replay reports both steps above as unowned.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, bootstrap_signal=false), service token "batch".

Cause 1 — fixture batch-job-definition-preserves-untagged-image, step task#4, operation DescribeJobDefinitions, verdict mismatch, expected_status 200, actual_status 200:
  $.jobDefinitions[0]: missing / extra (whole-object mismatch)
  expected containerProperties.networkConfiguration = {'assignPublicIp': 'ENABLED', 'interfaceConfigurations': []}
  actual   containerProperties.networkConfiguration = {'assignPublicIp': 'ENABLED'}
  expected containerProperties also carries 'vcpus': None and 'memory': None; actual containerProperties carries neither key.
  All other members equal on both sides (jobDefinitionName, jobDefinitionArn, revision 1, status ACTIVE, type container, parameters {}, image, executionRoleArn, volumes [], environment [], mountPoints [], ulimits [], resourceRequirements [VCPU 0.25, MEMORY 512], secrets [], fargatePlatformConfiguration {platformVersion LATEST}, tags {}, platformCapabilities [FARGATE], containerOrchestrationType ECS).
  divergence_key: service=batch|DescribeJobDefinitions|status=200->200|keys=$.jobDefinitions[0]

Cause 2 — fixture batch-job-definition-preserves-untagged-image, step teardown#6, operation DescribeJobDefinitions, verdict mismatch, expected_status 200, actual_status 200:
  $.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations: missing (expected [], got None)
  divergence_key: service=batch|DescribeJobDefinitions|status=200->200|keys=$.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations

Model evidence: smithy_query(shape="NetworkConfiguration") on services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json lists only member assignPublicIp -> AssignPublicIp; the interface-configuration list is recorded behavior only. smithy_query(shape="ContainerProperties") lists vcpus and memory as @deprecated Integer members.

No environment-specific value is part of the requirement here: the registry host/port in `image`, the generated job-definition and role names, and the request ids in these fixtures differ per run and are not asserted as requirements. The requirement is the member set of the serialized containerProperties.networkConfiguration.

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from the checkout.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay fixture batch-job-definition-preserves-untagged-image up to step task#4 — it registers a FARGATE container job definition with networkConfiguration.assignPublicIp=ENABLED and then calls DescribeJobDefinitions for that definition. Inspect jobDefinitions[0].containerProperties.networkConfiguration in the 200 response: the emulator returns only assignPublicIp, while the recorded response also carries interfaceConfigurations: [] (and carries the deprecated vcpus/memory keys).
4. Cause 2: continue the same fixture to step teardown#6, which calls DescribeJobDefinitions again for the same definition. Assert $.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations == [] — it is absent from the emulator's response.
5. Both steps are reported as verdict=mismatch with expected_status 200 / actual_status 200 and no owning issue.
