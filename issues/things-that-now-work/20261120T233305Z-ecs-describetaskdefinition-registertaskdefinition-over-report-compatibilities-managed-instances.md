Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults|run#18|AssertionError|assert sorted(got["compatibilities"]) == ["EC2", "FARGATE"], (
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# ECS DescribeTaskDefinition/RegisterTaskDefinition over-report compatibilities: MANAGED_INSTANCES is added to every revision unconditionally

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTaskDefinition

## Description
1 cause.

Cause 1 of 1: the derived `TaskDefinition$compatibilities` list always carries `MANAGED_INSTANCES`, for every registered revision, because the launch-type predicate that builds the list classifies that launch type as unconditionally compatible. Failing check: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults.

AWS: `TaskDefinition$compatibilities` is DERIVED by the service, never echoed from the request. Smithy: `com.amazonaws.ecs#TaskDefinition$compatibilities` targets `CompatibilityList`/`Compatibility`, whose value set is [EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES]; the member's documentation says only "Amazon ECS validates the task definition parameters with those supported by the launch type", so the model states which values exist but not the derivation rule — for the rule itself this is recorded behavior only. The check registers a Fargate-shaped revision (networkMode `awsvpc`, task-level cpu "2048" / memory "4096", requiresCompatibilities ["FARGATE"], an executionRoleArn, ephemeralStorage.sizeInGiB 40, a non-essential fluentbit FireLens router plus an essential service container) and the recorded AWS response reports `compatibilities` of exactly EC2 and FARGATE, with `requiresCompatibilities` unchanged at ["FARGATE"]. HTTP 200; MANAGED_INSTANCES is not reported merely because the revision uses awsvpc.

Emulator: the same DescribeTaskDefinition returns three launch types — ['EC2', 'FARGATE', 'MANAGED_INSTANCES'] — with HTTP 200. Suite text: `AssertionError: compatibilities are COMPUTED from the definition; got ['EC2', 'FARGATE', 'MANAGED_INSTANCES']` (expected ['EC2', 'FARGATE'] after sorting). Every other assertion in the check (container order and images, router firelensConfiguration/essential/memoryReservation/cpu, the injected router user "0" and no user on the service container, awsvpc portMappings gaining hostPort == containerPort, the option-less awsfirelens driver, task-level cpu/memory/networkMode/requiresCompatibilities/ephemeralStorage/status/revision, requiresAttributes) passed, so the divergence is confined to this one derived member. The fixed issue 20261025T062345Z-ecs-describetaskdefinition-registertaskdefinition-over-reports-compatibilities-managed-instances.md describes this same over-report and is marked fixed, but the check still fails: the current derivation reports MANAGED_INSTANCES for ALL revisions, not only awsvpc ones, so the behavior was not corrected (the sibling check under heading ecs__servicediscovery shows the same extra entry on a bridge-mode revision). The open wire issue 20261120T005727Z-ecs-task-definition-records-render-wrong-compatibilities-volumes.md asks for the OPPOSITE on its own fixtures (that MANAGED_INSTANCES be ADDED for an awsvpc Fargate revision); it does not cover this check, and the two expectations cannot both be met by an unconditional arm — Hypothesis: the real derivation depends on something outside the task definition (account/region enablement of ECS Managed Instances) rather than on networkMode, so a fix must reproduce the recorded per-definition list rather than hard-coding presence or absence for every revision.

Why: `compatibilities` is what a client, the console and deployment tooling read to decide which launch types a revision may be placed with. A client that offers or auto-selects a launch type from this list offers ECS Managed Instances for a revision real ECS never validated for it. The same derived list is also the gate the emulator uses to validate a requested `launchType`, so the over-report additionally makes RunTask/CreateService with launchType MANAGED_INSTANCES succeed for revisions AWS refuses — a behavior change, not just a rendered payload.

Fix: the task-definition projection's `compatibilities` derivation in the ECS service state — the MANAGED_INSTANCES arm of its per-launch-type predicate, which currently returns true for every revision; the same function is reused as the placement launch-type gate, so tightening it there fixes both the reported list and the gate. No other member of the projection needs to change.

Traces: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults. Code: services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state.rs — `COMPATIBILITY_ORDER` (line 1427) and `TaskDefinitionRecord::compatibilities()` (lines 3879-3899, the `"EC2" | "MANAGED_INSTANCES" => true` arm at line 3884), rendered by `TaskDefinitionRecord::view()` (line 3920) and reached from services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/register_task_definition.rs.

## Evidence
Check: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults (verdict: mismatch).

Request (RegisterTaskDefinition): family=<unique>, cpu="2048", memory="4096", networkMode="awsvpc", requiresCompatibilities=["FARGATE"], executionRoleArn=<role>, ephemeralStorage={"sizeInGiB": 40}, containerDefinitions=[{name:"log-router", image:"<registry>/<repo>:fluent-bit-latest", cpu:256, memoryReservation:512, essential:false, firelensConfiguration:{type:"fluentbit"}, logConfiguration:{logDriver:"awslogs", options:{...}}}, {name:"service", image:"<registry>/<repo>:1.0", cpu:1792, memory:4096, essential:true, portMappings:[{containerPort:8443, protocol:"tcp"}], logConfiguration:{logDriver:"awsfirelens"}}]. Then DescribeTaskDefinition(taskDefinition=<returned arn>).

Expected (recorded real AWS): HTTP 200, taskDefinition.compatibilities == ["EC2", "FARGATE"] (sorted), taskDefinition.requiresCompatibilities == ["FARGATE"].

Actual (emulator): HTTP 200, taskDefinition.compatibilities == ['EC2', 'FARGATE', 'MANAGED_INSTANCES'].

Suite failure text:
  File "check_ecs_ecsdeploy_firelens_task_definition_round_trips_with_router_defaults.py", line 54, in run
    assert sorted(got["compatibilities"]) == ["EC2", "FARGATE"], (
  AssertionError: compatibilities are COMPUTED from the definition; got ['EC2', 'FARGATE', 'MANAGED_INSTANCES']

The family name, registry/repository, role ARN and task-definition ARN above are the recorded run's values filling the template; the requirement is the CONTENTS of the derived list for a revision with these parameters, not those literals.

## Steps to Reproduce
1. Boot the emulator and point an ecs client at it.
2. RegisterTaskDefinition with networkMode="awsvpc", task-level cpu="2048" and memory="4096", requiresCompatibilities=["FARGATE"], an executionRoleArn, ephemeralStorage={"sizeInGiB":40}, and two container definitions (a non-essential fluentbit FireLens router plus an essential service container publishing containerPort 8443).
3. Call DescribeTaskDefinition on the returned taskDefinitionArn and read taskDefinition.compatibilities.
4. Observe ['EC2','FARGATE','MANAGED_INSTANCES']; real AWS reports only EC2 and FARGATE for this revision.
5. Secondary effect: RunTask or CreateService with launchType="MANAGED_INSTANCES" against the same revision is accepted, because the launch-type gate reads the same derived list.
6. Re-replay check ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults to confirm the fix.
