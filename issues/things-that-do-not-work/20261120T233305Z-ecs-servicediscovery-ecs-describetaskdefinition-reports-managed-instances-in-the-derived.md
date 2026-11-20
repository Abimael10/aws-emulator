Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__servicediscovery.ecs-task-definition-derives-compatibilities-beyond-what-was-required
Issue-Key: ecs__servicediscovery.ecs-task-definition-derives-compatibilities-beyond-what-was-required|run#5|AssertionError|assert sorted(td["compatibilities"]) == ["EC2", "EXTERNAL"], (
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# ECS DescribeTaskDefinition reports MANAGED_INSTANCES in the derived compatibilities of a bridge-mode EC2 task definition

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTaskDefinition

## Description
1 cause.

Cause 1 of 1: a task definition registered with `networkMode: bridge` and `requiresCompatibilities: ["EC2"]` is described with a derived `compatibilities` list that carries a third launch type, `MANAGED_INSTANCES`, which the revision's parameters do not support. Failing check: ecs__servicediscovery.ecs-task-definition-derives-compatibilities-beyond-what-was-required.

AWS: `TaskDefinition$compatibilities` is DERIVED from the stored revision (Smithy: `com.amazonaws.ecs#TaskDefinition$compatibilities` -> `CompatibilityList`/`Compatibility`, value set [EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES]; the member documents only "Amazon ECS validates the task definition parameters with those supported by the launch type", so the derivation rule itself is recorded behavior only). For a bridge-mode revision requiring [EC2] the recorded AWS response reports exactly the superset [EXTERNAL, EC2] — bridge networking rules FARGATE out, and no managed-instances entry appears — while `requiresCompatibilities` stays exactly ["EC2"]. HTTP 200.

Emulator: the same DescribeTaskDefinition returns ['EXTERNAL', 'EC2', 'MANAGED_INSTANCES'] with HTTP 200. Suite text: `AssertionError: compatibilities is ['EXTERNAL', 'EC2', 'MANAGED_INSTANCES']; real AWS DERIVES [EXTERNAL, EC2] from a bridge-mode definition, a superset of what was required`. This is the same root cause as the issue filed under service `ecs` for check ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults (filed separately because the suite lists the two checks under different service headings): the state's launch-type predicate treats MANAGED_INSTANCES as compatible with every revision, so the extra entry appears on awsvpc/Fargate and bridge/EC2 revisions alike. The fixed issue 20261025T062345Z-ecs-describetaskdefinition-registertaskdefinition-over-reports-compatibilities-managed-instances.md and the open wire issue 20261120T005727Z-ecs-task-definition-records-render-wrong-compatibilities-volumes.md both discuss this member but neither covers this check, and the latter asks for MANAGED_INSTANCES to be ADDED on its own awsvpc fixtures.

Why: a client that reads `compatibilities` to choose a launch type or a capacity provider for a bridge-mode revision concludes it may be placed on ECS Managed Instances capacity; real AWS refuses that placement. Because the emulator reuses the same derived list as its launchType gate, it also accepts RunTask/CreateService with launchType MANAGED_INSTANCES for this bridge revision. A client that reads the list positionally also reads the wrong launch type at index 2.

Fix: the task-definition projection's `compatibilities` derivation in the ECS service state — the MANAGED_INSTANCES arm of the per-launch-type predicate, which must be classified by the revision's own parameters (as the FARGATE and EXTERNAL arms are) instead of returning true for every revision; the launch-type validation helper that shares the function then stops admitting the launch type too.

Traces: ecs__servicediscovery.ecs-task-definition-derives-compatibilities-beyond-what-was-required. Code: services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state.rs — `TaskDefinitionRecord::compatibilities()` lines 3879-3899 (the `"EC2" | "MANAGED_INSTANCES" => true` arm at line 3884) and `COMPATIBILITY_ORDER` at line 1427.

## Evidence
Check: ecs__servicediscovery.ecs-task-definition-derives-compatibilities-beyond-what-was-required (verdict: mismatch).

Request (RegisterTaskDefinition): family=<unique>, networkMode="bridge", requiresCompatibilities=["EC2"], containerDefinitions=[{name:"<app>", image:"public.ecr.aws/docker/library/nginx:1.27", essential:true, cpu:128, memory:128, portMappings:[{containerPort:80, hostPort:0, protocol:"tcp"}]}]. Then DescribeTaskDefinition(taskDefinition=<family>).

Expected (recorded real AWS): HTTP 200, taskDefinition.compatibilities == ["EC2", "EXTERNAL"] (sorted) with no FARGATE and no MANAGED_INSTANCES entry, taskDefinition.requiresCompatibilities == ["EC2"].

Actual (emulator): HTTP 200, taskDefinition.compatibilities == ['EXTERNAL', 'EC2', 'MANAGED_INSTANCES'].

Suite failure text:
  File "check_ecs__servicediscovery_ecs_task_definition_derives_compatibilities_beyond_what_was_required.py", line 75, in run
    assert sorted(td["compatibilities"]) == ["EC2", "EXTERNAL"], (
  AssertionError: compatibilities is ['EXTERNAL', 'EC2', 'MANAGED_INSTANCES']; real AWS DERIVES [EXTERNAL, EC2] from a bridge-mode definition, a superset of what was required

The family and container names are the recorded run's generated values; the requirement is the contents of the derived list for a bridge-mode revision, not those literals.

## Steps to Reproduce
1. Boot the emulator and point an ecs client at it.
2. RegisterTaskDefinition with networkMode="bridge", requiresCompatibilities=["EC2"] and one essential container (cpu 128, memory 128, portMappings [{containerPort:80, hostPort:0, protocol:"tcp"}]).
3. DescribeTaskDefinition on that family and read taskDefinition.compatibilities.
4. Observe ['EXTERNAL','EC2','MANAGED_INSTANCES']; real AWS derives only [EXTERNAL, EC2] for a bridge-mode revision.
5. Re-replay check ecs__servicediscovery.ecs-task-definition-derives-compatibilities-beyond-what-was-required to confirm the fix.
