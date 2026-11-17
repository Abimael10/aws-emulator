Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: drs.wait-step-cannot-be-first-in-recovery-plan-rejected
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs.wait-step-cannot-be-first-in-recovery-plan-rejected|run#7|AssertionError|assert code == "ValidationException", (
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# drs CreateRecoveryPlanStep is a 501 scaffold stub, so the WAIT-step-cannot-be-first refusal (ValidationException) never happens

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRecoveryPlanStep

## Description
One net-new root cause in the Elastic Disaster Recovery (drs) emulator, grounded in one failing behavioral check of this suite.

Cause 1 of 1 — CreateRecoveryPlanStep is still an unimplemented scaffold, so every step creation — valid or invalid — answers HTTP 501 `NotImplemented`, and in particular the recovery-plan ordering rule that refuses a WAIT step in first position is never enforced.

AWS: CreateRecoveryPlanStep is modelled (Smithy: POST /CreateRecoveryPlanStep, success 201; input CreateRecoveryPlanStepRequest with required `recoveryPlanArn` (StrictDRSARN, @length 20..2048, pattern ^arn:aws(-[a-z0-9]+)*:drs:[a-z0-9-]+:[0-9]{12}:[a-zA-Z0-9_/.-]+$), required `stepName` (RecoveryPlanStepName, @length 1..256, pattern ^[a-zA-Z0-9][a-zA-Z0-9 _-]*$), required `configuration` (RecoveryPlanStepConfiguration — documented "Type-specific configuration for a recovery plan step. Exactly one member must be set."), optional `stepOrder` (RecoveryPlanStepOrder, @range 1..20) and an @idempotencyToken `clientToken`; output required `recoveryPlanStep`; modelled errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 402, ThrottlingException 429, UninitializedAccountException 400, ValidationException 400). A step is either SERVER type (servers recovered in parallel) or WAIT type (a timed pause between steps), and a WAIT step is only meaningful between two other steps. For a request whose `configuration` carries `waitStepConfiguration` and whose `stepOrder` places it first in the plan, AWS refuses with HTTP 400, error code `ValidationException`, message `A WAIT step cannot be the first step in a recovery plan.` — that message is entirely fixed text for this error class: it names neither the plan ARN, the step name nor the order value. Nothing is created: an independent ListRecoveryPlanSteps on the plan still returns an empty `recoveryPlanSteps`, and the plan itself remains readable by GetRecoveryPlan with its original `name`. A well-ordered step (a WAIT step not in first position, or a SERVER step) succeeds with 201 and the created `recoveryPlanStep`.

Emulator: `CreateRecoveryPlanStep::handle` ignores its input entirely and returns `Err(crate::scaffolded::Scaffolded::not_implemented("CreateRecoveryPlanStep"))`, i.e. HTTP 501 with error code `NotImplemented` and the message `operation CreateRecoveryPlanStep is scaffolded but not implemented in this emulator`, for every request — the first-position WAIT step above included. The handler file still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker. The replayed check reaches this call on a recovery plan the emulator DID create (CreateRecoveryPlan answered normally), so the plan surface is otherwise live; only step creation is absent. Note the open issue 20261109T163249Z-drs-4-net-new-behavioral-root-causes-the.md lists four other drs causes (the initialization gate, the stagingAreaSubnetId existence check, the out-of-enum error code, and the three tag operations) and the open stub issues name CreateLaunchConfigurationTemplate, TagResource and ListRecoveryPlans — none of them covers CreateRecoveryPlanStep.

Why: a client cannot build a recovery plan at all — the step-composition half of the recovery-plan API is missing — and the refusal path in particular is unreachable. Code that relies on AWS rejecting a malformed plan ordering (so it can surface the ordering rule to a user, or fall back to inserting the WAIT step later) instead receives a 501 with an error code no AWS SDK models for this operation: it is not a modelled ValidationException, so SDK error-shape matching and retry classification both miss it, and the client cannot distinguish "your plan ordering is invalid" from "the service is broken". Because 501 is also returned for perfectly valid steps, no recovery plan can be populated, and every downstream check of plan steps, ordering and execution is blocked.

Fix: implement the operation in its own handler file, services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/create_recovery_plan_step.rs — resolve `recoveryPlanArn` against the stored recovery plans, validate the step's position against the plan's existing steps BEFORE any state write (refusing a WAIT-type `configuration` at first position with the modelled ValidationException and the message above, leaving the step store unchanged), otherwise persist the step and return it with 201 — and delete the file's SCAFFOLDED marker line when done.

Traces: drs.wait-step-cannot-be-first-in-recovery-plan-rejected.

## Evidence
Behavioral suite: drs 2/15 (13%).

drs.wait-step-cannot-be-first-in-recovery-plan-rejected [mismatch]
  request:  create_recovery_plan(name=<unique>) -> ok; then create_recovery_plan_step(recoveryPlanArn=<that plan's ARN>, stepName='sim2real-wait-step', stepOrder=1, configuration={'waitStepConfiguration': {'waitDurationMinutes': 5}})
  expected: HTTP 400, error code `ValidationException`, message `A WAIT step cannot be the first step in a recovery plan.` (fixed text for this error class — no request- or state-derived part); nothing created, so ListRecoveryPlanSteps(recoveryPlanArn=<plan>) returns recoveryPlanSteps == [] and GetRecoveryPlan still reports the plan's original name.
  actual:   HTTP 501, error code `NotImplemented`, message `operation CreateRecoveryPlanStep is scaffolded but not implemented in this emulator`
            AssertionError: a WAIT step as the first step of a recovery plan must raise ValidationException, got 'NotImplemented'
            (check_drs_wait_step_cannot_be_first_in_recovery_plan_rejected.py, line 40)

Source: services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/create_recovery_plan_step.rs:4 (the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker) and :180, where `handle` returns `Err(crate::scaffolded::Scaffolded::not_implemented("CreateRecoveryPlanStep"))` after discarding `input` at :170.

Model (services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/smithy-model/com-amazonaws-drs-elasticdisasterrecoveryservice.json): CreateRecoveryPlanStep POST /CreateRecoveryPlanStep, success code 201, declares ValidationException with httpError 400; `configuration` is required and RecoveryPlanStepConfiguration is documented "Exactly one member must be set."; `stepOrder` is @range 1..20.

## Steps to Reproduce
Boot the emulator and point a botocore `drs` client at it (region/account as the suite configures them).

1. plan = drs.create_recovery_plan(name='sim2real-drs-rp-waitfirst-<unique>')['recoveryPlan']; take arn = plan['recoveryPlanArn'] (this call succeeds today).
2. drs.create_recovery_plan_step(recoveryPlanArn=arn, stepName='sim2real-wait-step', stepOrder=1, configuration={'waitStepConfiguration': {'waitDurationMinutes': 5}})
3. Observe HTTP 501 with error code `NotImplemented` and the message `operation CreateRecoveryPlanStep is scaffolded but not implemented in this emulator`, instead of HTTP 400 `ValidationException` with `A WAIT step cannot be the first step in a recovery plan.`
4. Also issue a legitimate step on the same plan (e.g. a SERVER-type step, or the same WAIT step with stepOrder=2 after a first step exists) and observe the same 501 — valid step creation is unavailable too.
5. Confirm the post-conditions the check asserts: drs.list_recovery_plan_steps(recoveryPlanArn=arn)['recoveryPlanSteps'] must be [] after the refused call, and drs.get_recovery_plan(recoveryPlanArn=arn)['recoveryPlan']['name'] must still be the created name.
6. Re-replay drs.wait-step-cannot-be-first-in-recovery-plan-rejected.
