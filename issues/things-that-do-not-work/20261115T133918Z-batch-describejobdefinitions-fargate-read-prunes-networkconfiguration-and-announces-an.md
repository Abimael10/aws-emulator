Issue-Source: trace-sweep-divergence
Issue-Trace: batch-job-definition-preserves-untagged-image
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: batch-job-definition-preserves-untagged-image
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch|DescribeJobDefinitions|2xx->2xx|$.jobDefinitions[*].containerProperties.image
Issue-Related: 20261115T005730Z-batch-describejobdefinitions-omits-networkconfiguration-members-aws-returns-for.md
Issue-Commit: b5d0ebc6502388b6804721a67a38a04334f3e770

# DescribeJobDefinitions Fargate read prunes networkConfiguration and announces an unserved ECR registry host (2 causes)

> _Note: this issue was identified against the code base at commit `b5d0ebc6502388b6804721a67a38a04334f3e770`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobDefinitions

## Description
Reading back a registered FARGATE container job definition diverges from AWS in two independent ways, both authored by the batch job-definition read path. The replay reports both clustered steps as UNOWNED (owned_by empty, stub_path empty), so neither is covered by an existing issue's replay key.

Cause 1 of 2: DescribeJobDefinitions omits containerProperties.networkConfiguration.interfaceConfigurations (and the deprecated vcpus/memory members) for a Fargate container job definition — on the read right after registration (fixture batch-job-definition-preserves-untagged-image, step task#4) and still on the later read of the same definition (same fixture, step teardown#6).
AWS: recorded behavior only. The on-disk Smithy model (services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json, com.amazonaws.batch#NetworkConfiguration) declares only the member `assignPublicIp` -> AssignPublicIp and says nothing about an interface-configuration list, so the requirement comes from the recording: the recorded 200 response returns containerProperties.networkConfiguration as a structure carrying BOTH assignPublicIp and interfaceConfigurations, with interfaceConfigurations serialized as an EMPTY LIST when the registration supplied no interface configurations. The recorded containerProperties for the same Fargate definition also carries the keys `vcpus` and `memory` (Smithy: ContainerProperties.vcpus and ContainerProperties.memory, both @deprecated "This field is deprecated, use resourceRequirements instead." and both documented as unsupported for jobs running on Fargate), present with no value. Status is 200 on both sides and no error code or message is part of this cause.
Emulator: answers 200, but serializes networkConfiguration with assignPublicIp only — the interface-configuration list is absent, not empty. At task#4 the whole jobDefinitions[0] object therefore mismatches (replay detail: `$.jobDefinitions[0]: missing/extra`, expected containerProperties.networkConfiguration = {'assignPublicIp': 'ENABLED', 'interfaceConfigurations': []} vs actual {'assignPublicIp': 'ENABLED'}, and expected containerProperties additionally carries vcpus/memory keys the actual object does not have). At teardown#6 the same gap is isolated to exactly one path: `$.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations: missing (expected [], got None)`. Every other member of the definition agrees on both sides (name, ARN, revision, status ACTIVE, type container, parameters, image, executionRoleArn, volumes, environment, mountPoints, ulimits, resourceRequirements, secrets, fargatePlatformConfiguration, tags, platformCapabilities, containerOrchestrationType).
Why: a client that reads a job definition back to inspect, diff or re-submit its networking sees the member as unset instead of as a present-but-empty list. Code that iterates networkConfiguration.interfaceConfigurations without a null guard faults; a register-if-changed reconciliation loop computes a spurious difference against desired state on every pass and re-registers a revision that did not change. The gap does not heal after the first read, so no client-side retry recovers it.
Fix: the DescribeJobDefinitions output serializer for ContainerProperties / NetworkConfiguration in the batch service crate — it must emit networkConfiguration's full reported member set, serializing the interface-configuration list as an empty list when the stored definition holds no entries, instead of skipping empty collections; the same pruning of the deprecated ContainerProperties members belongs in the same serializer.
Traces: fixture batch-job-definition-preserves-untagged-image, steps task#4 and teardown#6 (both DescribeJobDefinitions, expected 200, got 200, verdict mismatch).

Cause 2 of 2: the job definition's containerProperties.image is served with an ECR registry hostname the emulator does not serve, so the read publishes an address other than the one the emulator is reached on (fixture batch-job-definition-preserves-untagged-image, step teardown#6).
AWS: recorded behavior only — in the recording the image string resolves to a registry that really exists for the recorded account/region, so a client can pull it; the requirement is that the registry host named in a read-back job definition is a host that actually serves the registry the definition was registered against. The status is 200 and no error code or message is involved.
Emulator: answers 200 with containerProperties.image naming a production-style ECR registry endpoint of the form <account-id>.dkr.ecr.<region>.amazonaws.com/<repository-name> — a host the emulator does not serve — and the replay flags the member with verdict `host_not_own` on `$.jobDefinitions[0].containerProperties.networkConfiguration`-sibling path `$.jobDefinitions[0].containerProperties.image` (the string's literal text matches the recording; what diverges is that the announced host is not one the emulator answers on). The value originates in the emulator's own ECR repositoryUri, which the fixture registers as the job definition's image, so the emulator authored the unserved address.
Why: a consumer of a job definition uses image to pull — a container runtime, a policy check, or a human reproducing the job. Against the emulator that host resolves to real AWS or to nothing, so the pull leaves the emulated environment or fails, while every other read of the definition looks healthy. The untagged-image round-trip this fixture exists to prove therefore holds textually but not operationally.
Fix: the host/endpoint rewriting applied when the batch job-definition read path (and the ECR repositoryUri it is fed from) serializes a registry address — reads must publish the registry endpoint the emulator actually serves rather than a production amazonaws.com registry host.
Traces: fixture batch-job-definition-preserves-untagged-image, step teardown#6 (DescribeJobDefinitions, expected 200, got 200, verdict mismatch, key `$.jobDefinitions[0].containerProperties.image`, detail `host_not_own`).

Related but NOT covering these: open issue 20261115T005730Z-batch-describejobdefinitions-omits-networkconfiguration-members-aws-returns-for.md describes the same networkConfiguration omission but the replay reports both steps above as unowned, so its replay key does not gate them; 20261104T041055Z-batch-ecr-lambda-logs-a-cfn-created-ecr.md and 20261112T140115Z-batch-a-fargate-job-never-reaches-succeeded-and.md cover empty-collection omissions on DescribeComputeEnvironments/DescribeJobs, not DescribeJobDefinitions; 20261115T024314Z-batch-ecr-lambda-logs-ecr-repositoryuri-is-not-region-qualified-so-the.md covers region-qualification of the ECR URI, not the unserved host in cause 2.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score_pct 86.7, 21 of 165 steps mismatched). Service token "batch" (bootstrapped).

Cause 1, divergence 10 — fixture batch-job-definition-preserves-untagged-image, step task#4, operation DescribeJobDefinitions, verdict mismatch, expected_status 200, actual_status 200, owned_by [], stub_path "":
  $.jobDefinitions[0]: missing / extra (whole-object mismatch)
  expected containerProperties.networkConfiguration = {'assignPublicIp': 'ENABLED', 'interfaceConfigurations': []}
  actual   containerProperties.networkConfiguration = {'assignPublicIp': 'ENABLED'}
  expected containerProperties additionally carries 'vcpus' and 'memory' keys; actual carries neither.
  divergence_key: service=batch|DescribeJobDefinitions|status=200->200|keys=$.jobDefinitions[0],$.jobDefinitions[0].containerProperties.image

Cause 1 (second step) and Cause 2, divergence 11 — same fixture, step teardown#6, operation DescribeJobDefinitions, verdict mismatch, expected_status 200, actual_status 200, owned_by [], stub_path "":
  $.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations: missing (expected [], got None)
  $.jobDefinitions[0].containerProperties.image: host_not_own
  divergence_key: service=batch|DescribeJobDefinitions|status=200->200|keys=$.jobDefinitions[0].containerProperties.image,$.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations

Model evidence: smithy_query(query="shape", shape="NetworkConfiguration,ContainerProperties", model_path="services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json") — NetworkConfiguration declares only assignPublicIp -> AssignPublicIp (no interface-configuration member), so the interface-configuration list is recorded behavior only; ContainerProperties declares vcpus and memory as @deprecated Integer members, both documented as not supported for Fargate.

Not part of the requirement: the generated job-definition, repository and role names, the account id and region in the ARNs, the registry host's literal text, and the per-run request ids all differ per environment. The requirements asserted here are (1) the member set of the serialized containerProperties.networkConfiguration, including the empty interface-configuration list, and (2) that the registry host published in containerProperties.image is one the emulator serves.

## Steps to Reproduce
1. git_clone the project repo (branch bound to this run) and build/boot the emulator from that checkout.
2. Replay the wire fixture corpus for trace set simparity-wire-aws-v1.
3. Cause 1, step A: replay fixture batch-job-definition-preserves-untagged-image to step task#4. The fixture creates an ECR repository, then RegisterJobDefinition for a FARGATE container definition with networkConfiguration.assignPublicIp=ENABLED and resourceRequirements VCPU/MEMORY, then calls DescribeJobDefinitions for that definition. Inspect jobDefinitions[0].containerProperties.networkConfiguration in the 200 response: the emulator returns only assignPublicIp, while the recording also carries interfaceConfigurations: [] (and the deprecated vcpus/memory keys).
4. Cause 1, step B: continue the same fixture to step teardown#6, which calls DescribeJobDefinitions again for the same definition, and assert $.jobDefinitions[0].containerProperties.networkConfiguration.interfaceConfigurations == [] — it is absent, confirming the omission is unconditional in the serializer rather than an artifact of the registration call.
5. Cause 2: at the same teardown#6 response, take jobDefinitions[0].containerProperties.image and check the registry host it names against the host/port the emulator is actually reached on: the emulator publishes an <account>.dkr.ecr.<region>.amazonaws.com registry endpoint it does not serve (replay verdict host_not_own on that path), so a pull driven by that value never reaches the emulator.
6. Both steps are reported verdict=mismatch, expected_status 200 / actual_status 200, with no owning issue and no blocking stub.
