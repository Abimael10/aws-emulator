Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: imagebuilder.retry-image-non-failed-state-rejected
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder.retry-image-non-failed-state-rejected|_error#15|AssertionError|raise AssertionError("expected a ClientError, call succeeded unexpectedly")
Issue-Commit: 19f0e5a096ed637da91415d21b3a547e7492acec

# imagebuilder RetryImage accepts a CANCELLED image build and re-runs it, where AWS refuses with InvalidParameterValueException naming the unsupported build phase

> _Note: this issue was identified against the code base at commit `19f0e5a096ed637da91415d21b3a547e7492acec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RetryImage

## Description
Cause 1 of 1: RetryImage's eligibility rule is modelled on the image's STATE (FAILED or CANCELLED) instead of on the build PHASE the build stopped in, so a build that was cancelled is accepted and silently re-run. Failing check: imagebuilder.retry-image-non-failed-state-rejected.

AWS: RetryImage only resumes a build that failed in a retryable phase. Recorded behavior (the Smithy model declares no error for this case; RetryImage's modeled errors are CallRateLimitExceeded/Client/Forbidden/IdempotentParameterMismatch/InvalidRequest/ResourceInUse/Service/ServiceUnavailable, and its `imageBuildVersionArn` doc only says "The image must be in the FAILED or CANCELLED state") shows AWS REFUSING RetryImage on an image whose state is CANCELLED with HTTP 400, error code `InvalidParameterValueException`, and the message template:
  "The value supplied for parameter 'imageArn' is not valid. The provided image cannot be retried. Retry is not supported for images that failed during '<PHASE>' phase. Supported phases: TESTING, DISTRIBUTING"
Fixed text: everything except <PHASE> — i.e. the prefix "The value supplied for parameter 'imageArn' is not valid. The provided image cannot be retried. Retry is not supported for images that failed during '" and the suffix "' phase. Supported phases: TESTING, DISTRIBUTING". The single variable part is <PHASE>, the build phase the build was in when it stopped (taken from the emulator's own state for that build version — e.g. the build phase a cancel landed in); it is NOT a literal from any one run. Note the parameter the message names is 'imageArn', not the modeled input member name `imageBuildVersionArn`. Hypothesis: AWS derives retryability from the phase recorded on the build (only TESTING and DISTRIBUTING are resumable), which is why a cancel during BUILDING is never retryable even though the state is CANCELLED.

Emulator: the call SUCCEEDS (HTTP 200, RetryImageOutput echoing clientToken + imageBuildVersionArn) and the build version is re-run in place. `RETRYABLE_IMAGE_STATUSES = [FAILED, CANCELLED]` makes every CANCELLED build retryable, so the state check in RetryImage::handle never fires for it. Separately, when that check DOES fire (any other status) the emulator serves its own sentence, "The value supplied for parameter 'imageArn' is not valid. The provided image is not in a failed or cancelled state and cannot be retried.", which is neither AWS's clause nor AWS's phase/supported-phases wording — so even the refusal path this cause fixes has to be re-worded, not just re-gated.

Why: a client that treats RetryImage as the way to resume a build cannot distinguish a resumable build from an unresumable one. On AWS a cancelled build is a dead end and the 400 tells the caller which phases are resumable; against the emulator the retry is accepted, the image leaves its CANCELLED terminal state and starts producing a new output AMI, so retry/cleanup logic branching on InvalidParameterValueException takes the success path and leaks a build (and its AMI) that AWS would never have started. Error-handling code for this refusal is therefore never exercised.

Fix: the retry-eligibility rule and its refusal message in the RetryImage handler and this service's image state — services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/retry_image.rs (the RETRYABLE_IMAGE_STATUSES gate) and services/service-com-amazonaws-imagebuilder-imagebuilder/src/state.rs (RETRYABLE_IMAGE_STATUSES, image_not_retryable_message). Retryability must be decided from the phase the build stopped in (resumable only for TESTING and DISTRIBUTING), which requires the store to record that phase on the build version, and the refusal must carry AWS's message template above with the phase interpolated. Related but distinct open/closed work: 20261115T222854Z-imagebuilder-image-builder-createimage-builds-produce-no-output-ami.md (fixed) made RetryImage answer instead of 501 — it did not add the phase-based eligibility rule, and this check now fails on an unexpected SUCCESS rather than a refusal; 20261115T043558Z-ec2-iam-imagebuilder-image-builder-image-builds-never-run-startimagepipelineexecution-lands.md covers builds never passing through PENDING/BUILDING and does not cover RetryImage's eligibility.

Traces: imagebuilder.retry-image-non-failed-state-rejected — CreateImage, CancelImageCreation, poll GetImage to CANCELLED, then RetryImage, which the emulator answers 200 and AWS refuses 400 InvalidParameterValueException.

## Evidence
Check: imagebuilder.retry-image-non-failed-state-rejected [mismatch]

Assertion from the replay:
  File "check_imagebuilder_retry_image_non_failed_state_rejected.py", line 45, in run
    code, message = _error(lambda: ib.retry_image(imageBuildVersionArn=image_arn, clientToken=str(uuid.uuid4())))
  File "check_imagebuilder_retry_image_non_failed_state_rejected.py", line 75, in _error
    raise AssertionError("expected a ClientError, call succeeded unexpectedly")
  AssertionError: expected a ClientError, call succeeded unexpectedly

Expected (AWS, recorded): HTTP 400, Error.Code = InvalidParameterValueException, Error.Message matching
  head = "The value supplied for parameter 'imageArn' is not valid. The provided image cannot be retried. Retry is not supported for images that failed during '"
  tail = "' phase. Supported phases: TESTING, DISTRIBUTING"
with only the build phase between head and tail varying, and the image remaining CANCELLED afterwards.

Actual (emulator): HTTP 200 with RetryImageOutput {clientToken, imageBuildVersionArn} and the build re-run in place; no ClientError is raised at all.

Code path: services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/retry_image.rs:247 — `if !crate::state::RETRYABLE_IMAGE_STATUSES.contains(&status.as_str())` — with services/service-com-amazonaws-imagebuilder-imagebuilder/src/state.rs:574 `pub const RETRYABLE_IMAGE_STATUSES: &[&str] = &[IMAGE_STATUS_FAILED, IMAGE_STATUS_CANCELLED];` so CANCELLED passes the gate and retry_image.rs:258 `store.rerun_image_build(...)` mutates the build out of its terminal state. The refusal message that the gate would serve is services/service-com-amazonaws-imagebuilder-imagebuilder/src/state.rs:4273 `image_not_retryable_message()` = parameter_value_mismatch("imageArn", "The provided image is not in a failed or cancelled state and cannot be retried."), which does not match AWS's phase-based sentence.

Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json): RetryImage PUT /RetryImage, success 200; input imageBuildVersionArn (required, ARN pattern) + clientToken (required, len 1..64, @idempotencyToken); modeled errors do not include InvalidParameterValueException (the emulator widens it in via extra_errors), so the error code and message above are grounded in the recorded behavior the check pins.

## Steps to Reproduce
1. Create an IAM role + instance profile, then CreateInfrastructureConfiguration with that instanceProfileName.
2. CreateImageRecipe with parentImage arn:aws:imagebuilder:<region>:aws:image/amazon-linux-2-x86/x.x.x, semanticVersion 1.0.0.
3. CreateImage(infrastructureConfigurationArn, imageRecipeArn, imageTestsConfiguration={imageTestsEnabled: false}) and keep the returned imageBuildVersionArn.
4. CancelImageCreation(imageBuildVersionArn, clientToken=<uuid>) and poll GetImage until image.state.status == "CANCELLED".
5. Call RetryImage(imageBuildVersionArn=<that ARN>, clientToken=<uuid>).
   - AWS: HTTP 400 InvalidParameterValueException, message "The value supplied for parameter 'imageArn' is not valid. The provided image cannot be retried. Retry is not supported for images that failed during '<phase>' phase. Supported phases: TESTING, DISTRIBUTING"; the image stays CANCELLED.
   - Emulator: HTTP 200 with clientToken/imageBuildVersionArn echoed, and the build is re-run in place.
6. Re-run the suite check imagebuilder.retry-image-non-failed-state-rejected to confirm the fix (it also asserts the image is still CANCELLED after the refusal and that TagResource/ListTagsForResource on the same image still work).
