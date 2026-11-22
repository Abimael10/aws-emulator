Issue-Source: trace-sweep-divergence
Issue-Trace: autoscalingplans-create-scaling-plan-validation-envelopes
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscalingplans-create-scaling-plan-validation-envelopes
Issue-Shape-Id: com.amazonaws.autoscalingplans#AnyScaleScalingPlannerFrontendService
Issue-Key: autoscaling-plans|CreateScalingPlan|4xx->2xx|$.Message
Issue-Key: autoscaling-plans|CreateScalingPlan|4xx->2xx|$.__type
Issue-Key: autoscaling-plans|CreateScalingPlan|4xx->2xx|status
Issue-Commit: 7b83df792075b2a128316f9b5135ddba9a81dab1

# CreateScalingPlan accepts a ScalingInstruction with MinCapacity below 1 instead of 400 ValidationException 'Minimum capacity cannot be less than 1'

> _Note: this issue was identified against the code base at commit `7b83df792075b2a128316f9b5135ddba9a81dab1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateScalingPlan

## Description
One net-new wire cause in the autoscaling-plans crate.

Cause 1 of 1: CreateScalingPlan does not enforce a per-member lower bound on ScalingInstruction$MinCapacity, so an instruction whose minimum capacity is below the service's floor is ACCEPTED and a plan is minted, where AWS refuses the whole create with ValidationException.

AWS: Smithy (model services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/smithy-model/com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice.json) — CreateScalingPlan.ScalingInstructions is @required and the operation binds ValidationException (errorKind client, httpError 400, documented "An exception was thrown for a validation issue. Review the parameters provided."); ScalingInstruction$MinCapacity is @required and targets ResourceCapacity, a plain integer with NO @range trait, documented only as "The minimum capacity of the resource." The model therefore states no numeric floor, so the floor itself is recorded behavior only: the recorded step shows a create whose instruction carries a minimum capacity under the floor answered HTTP 400 with the awsJson error envelope $.__type = 'ValidationException' and $.Message = 'Minimum capacity cannot be less than 1'. That whole sentence is FIXED text for this error class — no part of it is interpolated from the request or from the emulator's state, and the literal 1 in it is AWS's own wording of the rule, not a value echoed back from the request. A refused create must also leave no plan behind, so DescribeScalingPlans for that name answers an empty ScalingPlans list.

Emulator: the create succeeds — HTTP 200 carrying a ScalingPlanVersion, with neither $.__type nor $.Message present — and the plan is stored with the out-of-range minimum. The per-instruction validation only checks the CROSS-MEMBER relation: validate_scaling_instruction refuses an inverted band with "Maximum capacity cannot be less than minimum capacity" and asserts in its own comment that "neither member is bounded on its own", so no MinCapacity value is ever refused on its own account.

Why: a client that mis-computes or mis-signs a capacity floor gets a 200 and a scaling plan AWS would never have created, so the emulator's state and AWS's diverge permanently from that call onward — the bad floor then flows into the capacity band every derived target-tracking policy and every capacity forecast is computed from. Validation-and-repair code that keys on the modelled ValidationException to detect a bad capacity band never fires, and the service cannot be used as a negative-path fixture for this error class at all because the class never occurs.

Why this is not the already-open issue: the open behavioral issue 20261120T134203Z-autoscaling-plans-2-net-new-causes-createscalingplan-accepts.md describes the same missing floor from a behavioral check, but it is grounded in behavioral check ids (autoscaling-plans.asp-create-rejects-capacity-and-target-bounds) and carries NO wire replay key, so the replayed fixture step below is not covered by it and would be closed unrun. This issue exists to carry that replay key; a fixer closing both should expect one shared fix.

Fix: the shared scaling-instruction validation in the service's state module (validate_scaling_instruction, reached from the CreateScalingPlan and UpdateScalingPlan handlers through validate_scaling_instructions) must refuse a MinCapacity below the service's floor with ValidationException and AWS's fixed sentence, in addition to the existing inverted-band relation, and the refused create must record no plan. The create handler, the serializer and the error envelope need no change — ValidationException is already modelled at httpError 400.

Traces: fixture autoscalingplans-create-scaling-plan-validation-envelopes step task#4 (the one cluster member).

## Evidence
Replayed step autoscalingplans-create-scaling-plan-validation-envelopes#task#4, operation CreateScalingPlan, verdict mismatch (cluster key service=autoscaling-plans|CreateScalingPlan|status=400->200|keys=$.Message,$.__type,status; affected_count 1).

Expected (recorded AWS):
  HTTP 400
  $.__type  = 'ValidationException'
  $.Message = 'Minimum capacity cannot be less than 1'
The status, the error code and the message are all requirements. The message is entirely fixed text for this error class; it has no variable part drawn from the request or from state.

Actual (emulator):
  HTTP 200
  $.__type  missing (expected 'ValidationException', got None)
  $.Message missing (expected 'Minimum capacity cannot be less than 1', got None)
The response instead carries the CreateScalingPlanResponse with its @required ScalingPlanVersion, i.e. the plan was created.

Model evidence: CreateScalingPlan errors = [ConcurrentUpdateException 500/server, InternalServiceException 500/server, LimitExceededException 400/client, ValidationException 400/client]; ScalingInstruction$MinCapacity -> ResourceCapacity (integer, required, no @range); ValidationException members = {Message -> ErrorMessage}, httpError 400.

Emulator source: services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/src/state.rs:1424-1430 — validate_scaling_instruction checks only `instruction.max_capacity < instruction.min_capacity` and comments that "neither member is bounded on its own"; no per-member floor remains. It is reached from the write paths via validate_scaling_instructions at src/state.rs:1376-1399. The sibling cross-member refusal in the same function works, which locates the defect precisely at the missing per-member floor rather than at the existence of instruction validation.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire fixture autoscalingplans-create-scaling-plan-validation-envelopes; step task#4 mismatches.
2. Or directly: call autoscaling-plans CreateScalingPlan with a valid ScalingPlanName and ApplicationSource and a single ScalingInstruction naming a valid resource triple (ServiceNamespace / ResourceId / ScalableDimension), one predefined target-tracking configuration, a MaxCapacity above the floor, and a MinCapacity BELOW the service's minimum of 1 (e.g. a negative minimum).
3. Observe HTTP 200 with a ScalingPlanVersion instead of HTTP 400 with $.__type 'ValidationException' and $.Message 'Minimum capacity cannot be less than 1'.
4. Call DescribeScalingPlans for that plan name and observe the plan the refused create should never have recorded.
5. Controls: re-run with MinCapacity equal to the floor to confirm the accepted case stays accepted, and with MaxCapacity < MinCapacity to confirm the existing inverted-band refusal ('Maximum capacity cannot be less than minimum capacity') is unaffected.
