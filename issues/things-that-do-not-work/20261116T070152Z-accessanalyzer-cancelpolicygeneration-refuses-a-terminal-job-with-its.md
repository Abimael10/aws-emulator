Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: accessanalyzer.start-policy-generation-completes-then-cancel-terminal-rejected
Issue-Shape-Id: com.amazonaws.accessanalyzer#AccessAnalyzer
Issue-Key: accessanalyzer.start-policy-generation-completes-then-cancel-terminal-rejected|run#36|AssertionError|assert cancel_msg == (
Issue-Commit: 592b93fbaec935edd06bdfe9abbe351007288c9e

# accessanalyzer: CancelPolicyGeneration refuses a terminal job with its own invented ValidationException message instead of AWS's "Invalid jobId: <jobId>. Illegal status change from <status> to CANCELED"

> _Note: this issue was identified against the code base at commit `592b93fbaec935edd06bdfe9abbe351007288c9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelPolicyGeneration

## Description
Cause 1 of 1: CancelPolicyGeneration's refusal message for a job that has already reached a terminal status does not match AWS's wording. Failing check: accessanalyzer.start-policy-generation-completes-then-cancel-terminal-rejected.

AWS: Smithy (services/service-com-amazonaws-accessanalyzer-accessanalyzer/smithy-model/com-amazonaws-accessanalyzer-accessanalyzer.json) models CancelPolicyGeneration with input member `jobId` (@httpLabel, required, PUT /policy/generation/{jobId}) and exactly one client error relevant here, `ValidationException` (HTTP 400, "Validation exception error"); there is no ResourceNotFoundException or ConflictException on this operation, so the MESSAGE is the only thing distinguishing the refusal classes. The model does not state the message text, so this is recorded behavior: when the named job is in a terminal status it did not itself cancel, AWS refuses with HTTP 400 / code `ValidationException` and message `Invalid jobId: <jobId>. Illegal status change from <status> to CANCELED`. Fixed text: `Invalid jobId: `, `. Illegal status change from `, ` to CANCELED`. Variable parts: `<jobId>` is the id from the request, `<status>` is the job's current status read from the service's own state (SUCCEEDED or FAILED in this class; the recorded run happened to show SUCCEEDED — that value is not part of the template). Note the two neighbouring refusal templates the emulator already words correctly and must keep distinct from this one: a never-issued jobId is `Invalid jobId: <jobId> does not exist`, and re-cancelling an already-CANCELED job is accepted (the operation is @idempotent).

Emulator: returns ValidationException (correct code/status) but with message `jobId must name a policy generation job in status IN_PROGRESS, but <jobId> is in status <status>` — e.g. the replay observed `jobId must name a policy generation job in status IN_PROGRESS, but 4e861a13-d322-4e39-8873-0579a951c0d2 is in status SUCCEEDED`. This is an invented sentence: neither the fixed text nor the ordering matches AWS, and it omits the `Invalid jobId: <jobId>.` prefix and the `Illegal status change from X to CANCELED` phrasing entirely.

Why: CancelPolicyGeneration models no error code that separates "no such job" from "illegal status change", so a client can only tell the two apart by parsing the message. Against this emulator, any client (or operator runbook, log alert, or test) that matches AWS's `Illegal status change from ... to CANCELED` sees no match and either treats a terminal job as a nonexistent one, retries a cancel that can never succeed, or reports the failure as unknown. The status the job is actually in is also no longer reported in AWS's position/format.

Fix: the refusal-message constructor for this class in the service's state module — `policy_generation_not_cancelable(job_id, status)` — and nothing in the handler's control flow; the handler already selects this refusal in the right case (job found, not already canceled, not in progress). The `jobId` field name attached by `field_validation_exception` and the 400/ValidationException classification are already right; only the rendered message text changes.

Traces: accessanalyzer.start-policy-generation-completes-then-cancel-terminal-rejected (assertion on `cancel_msg` after the job reached SUCCEEDED on its own). Relevant code: services/service-com-amazonaws-accessanalyzer-accessanalyzer/src/state.rs lines 863-876 (`policy_generation_not_cancelable`), called from services/service-com-amazonaws-accessanalyzer-accessanalyzer/src/ops/cancel_policy_generation.rs lines 180-186.

## Evidence
Check: accessanalyzer.start-policy-generation-completes-then-cancel-terminal-rejected [mismatch]

AssertionError raised at the `assert cancel_msg == (f"Invalid jobId: {job_id}. Illegal status change from {natural_status} to CANCELED")` assertion; the reported actual value was:
  jobId must name a policy generation job in status IN_PROGRESS, but 4e861a13-d322-4e39-8873-0579a951c0d2 is in status SUCCEEDED

Expected (AWS), for a job in a terminal status, on PUT /policy/generation/{jobId}:
  HTTP 400, Error.Code = ValidationException
  Error.Message = "Invalid jobId: <jobId>. Illegal status change from <status> to CANCELED"
    fixed text: "Invalid jobId: " / ". Illegal status change from " / " to CANCELED"
    variable: <jobId> from the request, <status> from the job record's current status

Actual (emulator):
  HTTP 400, Error.Code = ValidationException (correct)
  Error.Message = "jobId must name a policy generation job in status IN_PROGRESS, but <jobId> is in status <status>"

Everything before this assertion in the check passed, including the two StartPolicyGeneration validation messages ("Missing cloudTrailDetails", "Invalid cloudTrailArn: ... Unknown trail: ..."), so the divergence is isolated to this one refusal template. Source of the emulator text: state.rs `policy_generation_not_cancelable` -> format!("jobId must name a policy generation job in status {JOB_STATUS_IN_PROGRESS}, but {job_id} is in status {status}").

## Steps to Reproduce
1. Start the emulator and point a botocore accessanalyzer client at it.
2. StartPolicyGeneration with policyGenerationDetails={principalArn: <an existing role arn>} and cloudTrailDetails referencing a trail that exists (accessRole, startTime, endTime, trails=[{cloudTrailArn, allRegions: true}]); capture jobId from the response.
3. Poll GetGeneratedPolicy(jobId=jobId) until jobDetails.status leaves IN_PROGRESS (the emulator completes it as SUCCEEDED).
4. Call CancelPolicyGeneration(jobId=jobId) and read the ClientError's Error.Code and Error.Message.
5. Observe code ValidationException with message "jobId must name a policy generation job in status IN_PROGRESS, but <jobId> is in status SUCCEEDED"; AWS answers "Invalid jobId: <jobId>. Illegal status change from SUCCEEDED to CANCELED".
6. Equivalently: replay check accessanalyzer.start-policy-generation-completes-then-cancel-terminal-rejected.
