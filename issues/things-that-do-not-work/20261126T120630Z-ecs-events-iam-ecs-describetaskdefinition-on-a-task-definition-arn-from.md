Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__events__iam.rule-and-task-definition-lookup-from-another-region-rejected
Issue-Key: ecs__events__iam.rule-and-task-definition-lookup-from-another-region-rejected|run#30|AssertionError|assert code == "InvalidParameterException", (
Issue-Commit: eef3054027f29376844deedfa2c9294c0516da9b

# ecs: DescribeTaskDefinition on a task-definition ARN from another Region is refused ClientException "Unable to describe task definition." instead of InvalidParameterException "Invalid task definition: Invalid Region in ARN"

> _Note: this issue was identified against the code base at commit `eef3054027f29376844deedfa2c9294c0516da9b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTaskDefinition

## Description
One net-new root cause. Cause count: 1. A fixer must close this one cause before submitting.

Not covered by any open issue: the ~100 open `ecs*` issues are about the task/service data plane (tasks never reaching RUNNING, state-change events, container sessions), the capacity-provider/cluster readers, or repositoryUri rendering; none of them names DescribeTaskDefinition's cross-Region ARN validation, and the failing check is marked neither "ALREADY FILED" nor "OWNED by" in the replay report. The nearest neighbours — 20261122T202428Z-ecs-deregister-and-delete-task-definition-responses-render.md and 20261124T210114Z-ecs-task-definition-compatibilities-can-never-derive-managed.md — are about the SHAPE of a successful task-definition response, not about this refusal, and carry neither this replay key.

=== Cause 1 of 1: a `taskDefinition` ARN whose Region component is not the endpoint's Region is treated as "no such revision here" instead of being rejected as an invalid ARN ===

AWS: ECS speaks awsJson1_1 (service com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113). Smithy: DescribeTaskDefinition takes `taskDefinition` (String, @required) — documented as "The family for the latest ACTIVE revision, family and revision (family:revision) for a specific revision in the family, or full Amazon Resource Name (ARN) of the task definition to describe" — and models AccessDeniedException (403), ClientException (400), InvalidParameterException (400) and ServerException (500). The model does not state the message text, so the message requirement is recorded behavior only: AWS parses the ARN and, when its Region field is not the Region of the endpoint the call was made to, refuses the REQUEST as malformed rather than reporting a missing resource — HTTP 400, `Error.Code` `InvalidParameterException`, `Error.Message` `Invalid task definition: Invalid Region in ARN`. That whole sentence is FIXED text: it carries nothing from the request, no Region name, no family, no revision, no account id. The distinction AWS draws is between the two modeled 400s: InvalidParameterException for "the parameter itself is not valid for this endpoint", ClientException for "the identifier is well-formed but names nothing". Note this is a per-ARN rule and not a blanket cross-Region blindness: the same check's sibling reads — ListTaskDefinitions(familyPrefix=...) answering `[]` and DescribeClusters answering a `MISSING` failure from the far Region — are both satisfied by the emulator, so only the ARN-validation branch diverges.

Emulator: the Region component of the ARN is never looked at. The handler parses the reference down to (family, revision) only, then resolves it in the Region the call was made in; the far-Region lookup finds nothing and the handler answers the generic not-found branch — HTTP 400, `Error.Code` `ClientException`, `Error.Message` `Unable to describe task definition.` (the fixed sentence `crate::state::TASK_DEFINITION_NOT_FOUND_MESSAGE`). So both the error code and the whole message sentence diverge, and an ARN that names another Region is indistinguishable from an ARN that names a deleted revision in this one.

Why: a client that calls ECS from the wrong Region with an ARN copied from another Region — the common CDK/console/cross-account copy-paste mistake, and exactly what tooling catches by code — is told the revision does not exist instead of that its ARN is for another Region. Code branching on `InvalidParameterException` to surface "you are pointed at the wrong Region" never fires; retry/fallback logic that treats ClientException "not found" as "register it again" will register a duplicate family in the wrong Region instead of failing fast. Any error-code assertion or message match written against real ECS fails.

Fix: the shared `taskDefinition`-reference validator on the read path — `crate::state::parse_task_definition_reference` / `crate::state::task_definition_reference_fault` and the fault-to-error mapping in the DescribeTaskDefinition handler. The parser must recognise the ARN form and surface a distinct "ARN names another Region" fault, which the handler maps to InvalidParameterException with the fixed sentence, BEFORE the store is consulted (the current code only reaches the not-found branch). The same reference is resolved by the operations that PUT a revision to work, so the fault belongs in the shared validator rather than in this one handler.

Traces: ecs__events__iam.rule-and-task-definition-lookup-from-another-region-rejected (assertion `code == "InvalidParameterException"` at check line 59, immediately followed by the message assertion on `Invalid task definition: Invalid Region in ARN`). Code citations: services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/describe_task_definition.rs lines 165-222 (the reference is parsed at 172-199 with no Region fault, and the far-Region miss falls into the ClientException not-found branch at 214-222); services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state.rs line 2927 (`TASK_DEFINITION_NOT_FOUND_MESSAGE = "Unable to describe task definition."`).

## Evidence
Verdict `mismatch`, listed under the suite heading `### ecs__events__iam`.

  File "check_ecs__events__iam_rule_and_task_definition_lookup_from_another_region_rejected.py", line 59, in run
  assert code == "InvalidParameterException", (
  AssertionError: DescribeTaskDefinition in us-east-2 on a us-west-2 ARN must be refused, got 'ClientException'

The check's next assertion, not reached, pins the message:
  expected = "Invalid task definition: Invalid Region in ARN"
  assert message == expected

EXPECTED (AWS): HTTP 400, Error.Code `InvalidParameterException`, Error.Message `Invalid task definition: Invalid Region in ARN` (the entire sentence is fixed text).
ACTUAL (emulator): HTTP 400, Error.Code `ClientException`, Error.Message `Unable to describe task definition.` (services/.../src/state.rs:2927, returned from describe_task_definition.rs:216-221).

Positive and negative controls in the same check that PASS, so the divergence is isolated to the ARN-validation branch: every lookup answers at home (DescribeRule, ListRules, ListTaskDefinitions, DescribeClusters), and from the far Region ListTaskDefinitions(familyPrefix) returns `[]`, DescribeClusters returns `clusters: []` with a `MISSING` failure, and EventBridge DescribeRule returns ResourceNotFoundException with its exact message.

The Region pair in the replay (home us-west-2, far us-east-2) and the generated family/ARN are per-run values filling the template; the requirement is the error code and the fixed sentence for any ARN whose Region field differs from the endpoint's.

## Steps to Reproduce
1. Pick two default-enabled Regions, HOME and OTHER (e.g. us-west-2 and us-east-2).
2. In HOME: ecs RegisterTaskDefinition(family=<tok>, containerDefinitions=[...]) and keep the returned `taskDefinitionArn` (it embeds HOME as its Region).
3. Positive control, in HOME: ecs DescribeTaskDefinition(taskDefinition=<that ARN>) -> 200 with the revision.
4. In OTHER: ecs DescribeTaskDefinition(taskDefinition=<the HOME ARN>).
5. Observe HTTP 400 with Error.Code `ClientException` and Error.Message `Unable to describe task definition.`. Real ECS answers HTTP 400 with Error.Code `InvalidParameterException` and Error.Message `Invalid task definition: Invalid Region in ARN`.
6. Contrast with a genuine miss, which must keep the current behavior: in HOME call ecs DescribeTaskDefinition(taskDefinition="<tok>:999") (a HOME-Region reference naming no revision) -> ClientException is correct there.
