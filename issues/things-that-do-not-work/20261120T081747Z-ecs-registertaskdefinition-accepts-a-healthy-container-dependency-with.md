Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-register-task-definition-healthy-dependency-without-health-check-rejected
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-register-task-definition-healthy-dependency-without-health-check-rejected
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|RegisterTaskDefinition|4xx->2xx|$.__type
Issue-Key: ecs|RegisterTaskDefinition|4xx->2xx|$.message
Issue-Key: ecs|RegisterTaskDefinition|4xx->2xx|status
Issue-Commit: 58a84f52bf4284d464bddc60006b86eef897b096

# ECS RegisterTaskDefinition accepts a HEALTHY container dependency with no health check on the dependency container

> _Note: this issue was identified against the code base at commit `58a84f52bf4284d464bddc60006b86eef897b096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTaskDefinition

## Description
RegisterTaskDefinition in the ECS emulator does not enforce the service-side rule that a container dependency whose condition is HEALTHY requires the DEPENDED-ON container to have a health check configured. One cause.

Cause 1 of 1: a task definition in which container A declares dependsOn=[{containerName: B, condition: HEALTHY}] while container B carries no healthCheck member registers successfully instead of being refused.
AWS: Smithy (com.amazonaws.ecs, AmazonEC2ContainerServiceV20141113): RegisterTaskDefinition.containerDefinitions -> ContainerDefinition.dependsOn -> ContainerDependency, whose members containerName and condition are both required and whose condition targets the ContainerCondition enum [START, COMPLETE, SUCCESS, HEALTHY]. The model documents the rule in ContainerDependency.condition: "HEALTHY - This condition validates that the dependent container passes its Docker health check before permitting other containers to start. This requires that the dependent container has health checks configured." The health check itself is ContainerDefinition.healthCheck -> HealthCheck (command required), which is an OPTIONAL member, so the pairing can only be enforced by the service, not by the model's own constraints. RegisterTaskDefinition's modeled client errors are ClientException (httpError 400, defaulted), InvalidParameterException (400), LimitExceededException (400) and AccessDeniedException (403). Recorded behavior: HTTP 400 with __type ClientException and message 'A dependency container with HEALTHY condition must have health check configured'. That message text is wholly fixed for this error class — it interpolates no name, id or value from the request or from emulator state — so it is the requirement verbatim, and the refusal mints no revision for the family.
Emulator: answers HTTP 200 and registers the revision. The replay reports `status: expected 400, got 200`, with `$.__type: missing (expected 'ClientException', got None)` and `$.message: missing (expected 'A dependency container with HEALTHY condition must have health check configured', got None)` — i.e. a success body where an error body is required.
Why: a client relying on ECS to catch this gets a definition that can never start, because at task startup the agent waits on a health check that will never be reported and the dependent container is never permitted to start; the failure moves from a deterministic 400 at registration time to a stuck task at run time. Two further consequences: a deploy pipeline or test that asserts the refusal passes wrongly against the emulator, and because the emulator persists the revision AWS would have rejected, the family's revision counter advances, so later legitimate registrations land on different revision numbers than on AWS and clients pinning `family:N` resolve to the wrong definition.
Fix: the RegisterTaskDefinition handler's validation — the cross-container dependsOn check that, for every ContainerDependency with condition HEALTHY, requires the named target container in the same containerDefinitions list to carry a healthCheck; it must refuse with 400 ClientException and the fixed message above before any revision is minted. This belongs with the other cross-container/cross-field RegisterTaskDefinition rules in the ECS service crate's register_task_definition operation handler (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/). NOTE: open issue 20261103T041326Z-ecs-registertaskdefinition-7-fargate-firelens-validation-and-representation.md bundles seven other RegisterTaskDefinition validation/representation gaps (FireLens router, Fargate GPU/host path, Fargate awslogs execution role, pidMode, Windows task size, cpu/memory normalisation, networkMode default). None of its causes is the HEALTHY-dependency rule and it does not carry this fixture step's replay key, so this divergence is filed separately; a fixer adding a general cross-field validation pass may close both together.
Traces: ecs-register-task-definition-healthy-dependency-without-health-check-rejected#task#1.

## Evidence
Wire replay, fixture ecs-register-task-definition-healthy-dependency-without-health-check-rejected, step task#1, operation RegisterTaskDefinition, verdict mismatch (divergence_key service=ecs|RegisterTaskDefinition|status=400->200|keys=$.__type,$.message,status):

Expected (recorded AWS response):
  HTTP 400
  $.__type  = 'ClientException'
  $.message = 'A dependency container with HEALTHY condition must have health check configured'
  (no taskDefinition minted for the family)

Actual (emulator):
  HTTP 200
  $.__type  missing (got None)
  $.message missing (got None)
  the task definition revision is registered and returned

Fixed vs variable parts of the required message: the entire string 'A dependency container with HEALTHY condition must have health check configured' is fixed text for this error class; it contains no request value, container name, ARN, id, limit or timestamp to interpolate.

Model citations: ContainerDependency.condition is a required ContainerCondition enum [START, COMPLETE, SUCCESS, HEALTHY] whose documentation states the HEALTHY condition "requires that the dependent container has health checks configured"; ContainerDefinition.healthCheck -> HealthCheck is optional; RegisterTaskDefinition errors include ClientException with httpError 400.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. Replay fixture ecs-register-task-definition-healthy-dependency-without-health-check-rejected and observe step task#1 diverge.
3. Equivalently, by hand: call ecs RegisterTaskDefinition with a family and two container definitions, where the first container declares dependsOn=[{"containerName": "<second-container-name>", "condition": "HEALTHY"}] and the second container declares NO healthCheck member.
4. Observe the emulator answers HTTP 200 with a registered taskDefinition revision. Required: HTTP 400 with {"__type": "ClientException", "message": "A dependency container with HEALTHY condition must have health check configured"} and no revision minted (ListTaskDefinitions on that familyPrefix stays empty).
5. Control: adding a healthCheck (e.g. command ["CMD-SHELL", "exit 0"]) to the depended-on container must register successfully — the refusal is about the pairing, not about dependsOn itself.
