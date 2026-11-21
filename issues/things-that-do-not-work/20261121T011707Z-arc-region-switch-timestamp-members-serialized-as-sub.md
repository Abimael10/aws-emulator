Issue-Source: trace-sweep-divergence
Issue-Trace: arc-region-switch-lambda-step-plan-execution-completes
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: arc-region-switch-lambda-step-plan-execution-completes
Issue-Shape-Id: com.amazonaws.arcregionswitch#ArcRegionSwitch
Issue-Key: arc-region-switch|GetPlanExecution|2xx->2xx|$.endTime
Issue-Key: arc-region-switch|GetPlanExecution|2xx->2xx|$.plan.updatedAt
Issue-Key: arc-region-switch|GetPlanExecution|2xx->2xx|$.startTime
Issue-Key: arc-region-switch|GetPlanExecution|2xx->2xx|$.stepStates[*].endTime
Issue-Key: arc-region-switch|GetPlanExecution|2xx->2xx|$.stepStates[*].startTime
Issue-Key: arc-region-switch|ListPlanExecutions|2xx->2xx|$.items[*].endTime
Issue-Key: arc-region-switch|ListPlanExecutions|2xx->2xx|$.items[*].startTime
Issue-Commit: c1e629eb7cbb6abd12d76d276265809665f4d5fd

# arc-region-switch timestamp members serialized as sub-millisecond epoch floats

> _Note: this issue was identified against the code base at commit `c1e629eb7cbb6abd12d76d276265809665f4d5fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPlanExecution

## Description
This bundle has 2 causes, both the same root fault in one service: every `Timestamp` member the ARC Region switch plan-execution read paths emit is serialized as a raw sub-millisecond epoch float taken straight off the host clock, instead of the epoch-seconds timestamp AWS puts on the wire for this protocol. The replay harness does not assert the literal instant for these members (expected is the placeholder `<shaped-not-asserted>`), so each listed field failed on its SHAPE, not on being a different moment in time. Both steps returned HTTP 200 with otherwise-correct bodies, so the fault is purely in timestamp serialization.

Cause 1 of 2: GetPlanExecution - fixture `arc-region-switch-lambda-step-plan-execution-completes`, step `task#9`.
AWS: Smithy model `com.amazonaws.arcregionswitch#ArcRegionSwitch` (version 2022-07-26), protocols `awsJson1_0` and `rpcv2Cbor`. `GetPlanExecutionResponse` members `startTime` (@required), `endTime`, `updatedAt` and `StepState.startTime` / `StepState.endTime` all target the plain `Timestamp` shape with NO `@timestampFormat` trait, so the awsJson1_0 default applies: `epoch-seconds`, which AWS emits with at most millisecond resolution (a JSON number with no more than three fractional digits, e.g. `1718303400.123`, and a bare integer when the instant has no sub-second component). `Plan.updatedAt` is the same plain `Timestamp`. The operation is `@readonly` and models only `AccessDeniedException` (403) and `ResourceNotFoundException` (404); no error is involved here - the step is a successful 200 whose body shape is wrong.
Emulator: returns 200 but serializes `$.startTime`, `$.endTime`, `$.stepStates[0].startTime`, `$.stepStates[0].endTime` and `$.plan.updatedAt` as epoch floats carrying seven fractional digits - sub-millisecond, finer than the protocol's timestamp resolution. The harness reports each of the five as `value_mismatch (expected '<shaped-not-asserted>')`, i.e. the value is not a well-shaped epoch-seconds timestamp. Hypothesis: the handler keeps execution/step/plan times as a raw `f64` of seconds since the epoch (a direct `SystemTime`/`Instant` conversion) and writes that float unrounded, rather than normalizing to millisecond-truncated epoch-seconds at the serialization boundary.
Why: a client that deserializes these members into an instant type gets a value whose precision the protocol does not define. SDKs and generated deserializers for awsJson1_0 epoch-seconds truncate or reject excess fractional precision, so round-tripping a plan execution's `startTime`/`endTime` through the emulator can shift the instant, and any client that compares a timestamp it read back against one it previously received (polling a plan execution to completion, diffing `updatedAt` to detect a change, computing an elapsed recovery time from `endTime - startTime`) sees spurious inequality or a drifting value. Code written against real ARC Region switch, where these are millisecond-resolution epoch-seconds, misbehaves only against the emulator.
Fix: the output serializer for the plan-execution read paths - the component that renders `Timestamp` members into the awsJson1_0 (and rpcv2Cbor) body for `GetPlanExecution`, including the nested `StepState` and `Plan` structures. Normalize every `Timestamp` member to the protocol's epoch-seconds form with at most millisecond resolution at serialization time; if execution/step times are stored as raw float seconds in state, truncate on the way in as well so `updatedAt` is stable across reads.
Traces: `arc-region-switch-lambda-step-plan-execution-completes#task#9` (five timestamp members diverged in one response).

Cause 2 of 2: ListPlanExecutions - fixture `arc-region-switch-lambda-step-plan-execution-completes`, step `task#10`.
AWS: `ListPlanExecutionsResponse.items` targets `AbbreviatedExecutionsList` of `AbbreviatedExecution`, whose `startTime` (@required), `endTime` and `updatedAt` members all target the same plain `Timestamp` shape with no `@timestampFormat` override - so, again, awsJson1_0 default `epoch-seconds` at millisecond resolution. The operation is `@readonly` and `@paginated` (`items`), and models only `AccessDeniedException` (403) and `ResourceNotFoundException` (404).
Emulator: returns 200 but serializes `$.items[0].startTime` and `$.items[0].endTime` as the same seven-fractional-digit epoch floats, reported as `value_mismatch (expected '<shaped-not-asserted>')`. The values are the same instants the `GetPlanExecution` response carried, confirming the defect is in how the stored execution times are rendered, not in the list path's own bookkeeping.
Why: the abbreviated list is the normal way a client enumerates executions and then fetches one by id. With over-precise timestamps the list view and any locally cached instant disagree, and a client that sorts or filters executions by `startTime`/`endTime`, or that uses the list's timestamps as a watermark for the next poll, works off values whose precision the protocol does not define.
Fix: the same serialization component, applied to the `AbbreviatedExecution` projection used by `ListPlanExecutions`. Fixing the shared `Timestamp` rendering (and the stored representation behind it) should close both causes; verify the list path does not carry its own separate timestamp formatting.
Traces: `arc-region-switch-lambda-step-plan-execution-completes#task#10` (two timestamp members diverged in one response).

Note: the only other open arc-region-switch issue, `arc-region-switch__cloudformation: AWS::ARCRegionSwitch::Plan is not a registered resource type`, is about CloudFormation resource-type registration and does not cover either cause here.

## Evidence
Replay of trace set simparity-wire-aws-v1: 17 steps, 15 passed, 2 mismatched, score 88.2%, emulator_up=true, bootstrap_signal=false. Both mismatches are verdict `mismatch` on service `arc-region-switch`, expected_status 200 / actual_status 200 - the divergence is in the response body only.

Cause 1 - fixture `arc-region-switch-lambda-step-plan-execution-completes`, step `task#9`, operation GetPlanExecution:
  $.endTime: value_mismatch (expected '<shaped-not-asserted>', got 1795223729.3687365)
  $.plan.updatedAt: value_mismatch (expected '<shaped-not-asserted>', got 1795223713.820029)
  $.startTime: value_mismatch (expected '<shaped-not-asserted>', got 1795223723.8385224)
  $.stepStates[0].endTime: value_mismatch (expected '<shaped-not-asserted>', got 1795223728.8687365)
  $.stepStates[0].startTime: value_mismatch (expected '<shaped-not-asserted>', got 1795223724.3385224)

Cause 2 - fixture `arc-region-switch-lambda-step-plan-execution-completes`, step `task#10`, operation ListPlanExecutions:
  $.items[0].endTime: value_mismatch (expected '<shaped-not-asserted>', got 1795223729.3687365)
  $.items[0].startTime: value_mismatch (expected '<shaped-not-asserted>', got 1795223723.8385224)

Expected (AWS): each of these members is an awsJson1_0 epoch-seconds timestamp with at most millisecond resolution - an integer when there is no sub-second component, otherwise a number with no more than three fractional digits. The particular instants above are per-run clock values and are NOT the requirement; the requirement is the resolution/format of the serialized timestamp.
Actual (emulator): epoch-seconds floats with seven fractional digits (sub-millisecond), i.e. the host clock value written out unrounded.
Model evidence: smithy_query on services/service-com-amazonaws-arcregionswitch-arcregionswitch/smithy-model/com-amazonaws-arcregionswitch-arcregionswitch.json - service protocols [awsJson1_0, rpcv2Cbor], no @timestampFormat on GetPlanExecutionResponse.{startTime,endTime,updatedAt}, StepState.{startTime,endTime}, or AbbreviatedExecution.{startTime,endTime,updatedAt}; all target com.amazonaws.arcregionswitch#Timestamp.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1) and watch fixture `arc-region-switch-lambda-step-plan-execution-completes`, which drives a plan whose Lambda step runs to completion.
3. Step task#9 calls GetPlanExecution with the plan ARN and execution id from the earlier steps of the fixture. Inspect the 200 response body: $.startTime, $.endTime, $.plan.updatedAt and $.stepStates[0].{startTime,endTime} come back as epoch-seconds numbers with seven fractional digits instead of at-most-millisecond resolution.
4. Step task#10 calls ListPlanExecutions for the same plan ARN. Inspect $.items[0].startTime and $.items[0].endTime: the same over-precise epoch floats.
5. After the fix, re-replay both steps (`arc-region-switch-lambda-step-plan-execution-completes#task#9` and `#task#10`) and confirm every Timestamp member in both responses serializes as epoch-seconds with no more than millisecond precision.
