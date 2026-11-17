Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-microvms-image-lifecycle
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-microvms-image-lifecycle
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda|DeleteMicrovmImageVersion|4xx->4xx|$.message
Issue-Key: lambda|DeleteMicrovmImageVersion|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: lambda|DeleteMicrovmImageVersion|4xx->4xx|status
Issue-Commit: d66b49b5d20b841ed555a4c1167817e25b409ab6

# lambda DeleteMicrovmImageVersion refuses an absent MicroVM image with 404 ResourceNotFoundException instead of AWS's 400 ValidationException

> _Note: this issue was identified against the code base at commit `d66b49b5d20b841ed555a4c1167817e25b409ab6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteMicrovmImageVersion

## Description
This issue carries 1 root cause, observed on replay of wire corpus simparity-wire-aws-v1 with an EMPTY stub_path and an EMPTY owned_by.

=== Cause 1 of 1: a version-addressed DELETE for an image this account holds no record of is answered in the wrong error class — wrong HTTP status, wrong error code, wrong message ===
Fixture lambda-microvms-image-lifecycle, step task#9 (DeleteMicrovmImageVersion).

AWS: Smithy DeleteMicrovmImageVersion is DELETE /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion} -> 200, idempotent, with imageIdentifier (@httpLabel, @required, MicrovmImageIdentifier, @length 1..256) and imageVersion (@httpLabel, @required, NonBlankString, @length 1..2048, pattern ^[^\s]+$); it declares both ValidationException (httpError 400) and ResourceNotFoundException (httpError 404), so the model permits either and does not decide which this condition takes. The model does not state the text, so for the status, the code and the message this is recorded behavior only: addressing a version of an image identifier that resolves to no image of the calling account is refused with
    HTTP 400
    x-amzn-errortype: ValidationException:http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/
    message: "MicroVM Image not found: UpdateRuntimeStateRequest(newState=UPDATING, failureReason=null)"
Fixed text: "MicroVM Image not found: ", followed by AWS's own rendering of the internal request it was performing — recorded as "UpdateRuntimeStateRequest(newState=UPDATING, failureReason=null)". That trailing rendering takes NO value from the request (not the identifier, not the version) and no clock or per-run value, so for this operation it is constant text rather than a variable part; the message notably does NOT name the image or the version the request addressed. (Hypothesis, for orientation only: AWS implements the version delete as an internal runtime-state update, which is why its refusal leaks that request's rendering; nothing in the model or the recording states this, and the requirement is only the status, the code and the message above.)

Emulator: the same step answers
    HTTP 404
    x-amzn-errortype: ResourceNotFoundException:http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/
    message: "MicroVMImage not found for MicroVMImage: <imageIdentifier>, Version: <imageVersion>"
(replay: 'MicroVMImage not found for MicroVMImage: arn:aws:lambda:us-west-2:000000000000:microvm-image:sim2real-probe-lambda-microvms-microvmimg-9a120e37-sim2real-probe-lambda-microvms-missing-9a120e37, Version: 1' — the ARN and the version string are this run's values filling the emulator's own template, not part of any requirement). Three things diverge at once: the status class (404 vs 400), the error code the client switches on (ResourceNotFoundException vs ValidationException), and the whole message sentence (different fixed text, and it substitutes two request values AWS's sentence does not carry). Only the Coral namespace in x-amzn-errortype agrees. The answer comes from the handler's authorization/existence verdict: services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/ops/delete_microvm_image_version.rs:182-191 maps crate::state::ImageAccess::Absent to the local `not_found(...)` helper (same file, lines 307-319, which builds ResourceNotFoundException) with the message from crate::state::version_address_image_not_found_message (src/state.rs:344-354, `format!("MicroVMImage not found for MicroVMImage: {identifier}, Version: {image_version}")`).

Why: a client cannot classify this failure the way it classifies it against AWS. Error handling for this API is written on the code and the status: a 404/ResourceNotFoundException is normally the idempotent, benign outcome a deleting client swallows and stops on ("deleting a version that has already been deleted succeeds without error"), while a 400/ValidationException is a bad-request outcome that fails a pipeline and is never retried. Against the emulator a delete aimed at a mistyped or already-removed IMAGE is reported as a resource-absence, so a cleanup job treats it as success and a request-validation branch that AWS reaches is dead code; a client that greps or asserts the refusal text matches nothing either, and one that parses the identifier out of the message gets a value AWS never puts there.

Fix: this operation's refusal path — the ImageAccess::Absent arm of DeleteMicrovmImageVersion's handler (src/ops/delete_microvm_image_version.rs:182-191) must answer the modelled ValidationException (400) with AWS's sentence for this refusal, instead of routing to the shared 404 helper. The message belongs in the service's message helpers beside the other recorded refusal texts (src/state.rs:338-413), as a new helper for this condition: the existing version_address_image_not_found_message is the text OTHER version-addressed reads answer with and must not be reworded for them. Do not change the version-missing-on-an-existing-image arm (lines 235-243), which the replay does not fault here.

Related, NOT covering this: open issue 20261116T060741Z-lambda-microvms-version-not-found-refusal-uses-the.md concerns the MESSAGE WORDING of the 404 answered when a resolved image holds no such version (UpdateMicrovmImage Version, behavioral corpus) — a different condition (the image exists there), a different error class (it keeps 404/ResourceNotFoundException as correct), and it carries no key for this fixture step. No open issue carries this replay key; the replay reports this cluster with an empty owned_by.

Traces: lambda-microvms-image-lifecycle#task#9.

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (emulator_up true, "/__admin/health answered 200 and the :37645 data listener accepts"; 13 fixtures replayed, 104 steps, 92 passed, 12 mismatched, score 88.5%, 10 divergence clusters, bootstrap_signal false). Expected = recorded AWS response, Actual = emulator.

Cluster — fixture lambda-microvms-image-lifecycle, step task#9, operation DeleteMicrovmImageVersion, verdict mismatch, expected_status 400, actual_status 404, service 'lambda', stub_path EMPTY, owned_by EMPTY:
  status: expected 400, got 404
  $.message: value_mismatch
      expected 'MicroVM Image not found: UpdateRuntimeStateRequest(newState=UPDATING, failureReason=null)'
      got      'MicroVMImage not found for MicroVMImage: arn:aws:lambda:us-west-2:000000000000:microvm-image:sim2real-probe-lambda-microvms-microvmimg-9a120e37-sim2real-probe-lambda-microvms-missing-9a120e37, Version: 1'
  $headers.x-amzn-errortype: shape_mismatch
      expected 'ValidationException:http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/'
      got      'ResourceNotFoundException:http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/'
  divergence_key: service=lambda|DeleteMicrovmImageVersion|status=400->404|keys=$.message,$headers.x-amzn-errortype,status

Requirement vs per-run values: the requirement is HTTP 400, error code ValidationException, and the message "MicroVM Image not found: " + AWS's internal-request rendering as recorded ('UpdateRuntimeStateRequest(newState=UPDATING, failureReason=null)'), which carries nothing from the request. The ARN and the version '1' in the emulator's message are this run's request values filling the emulator's own (wrong) template and are not part of any requirement.

Model grounding (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json): DeleteMicrovmImageVersion, DELETE /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}, success 200, @idempotent; input imageIdentifier @httpLabel @required MicrovmImageIdentifier @length 1..256, imageVersion @httpLabel @required NonBlankString @length 1..2048 pattern ^[^\s]+$; errors include ValidationException (httpError 400) and ResourceNotFoundException (httpError 404) — the model admits both and fixes neither the choice nor the text, hence 'recorded behavior only' for the status, the code and the message.

Code sightings (orientation only): src/ops/delete_microvm_image_version.rs:182-191 (ImageAccess::Absent -> not_found(version_address_image_not_found_message(...))), :235-243 and :266-275 (the version-missing arms, not faulted here), :307-319 (the local not_found helper building ResourceNotFoundException); src/state.rs:344-354 (version_address_image_not_found_message, the diverging sentence) and src/state.rs:338-413 (the service's recorded refusal-message helpers, where the correct sentence belongs).

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Build an imageIdentifier in this account's own MicroVM-image namespace that names no existing image — e.g. arn:aws:lambda:<region>:<account>:microvm-image:<a name nothing was created under> (the fixture derives it from a created image's name plus a '-missing-<suffix>' tail, so the identifier is well-formed and own-account but resolves to no record).
3. Call DeleteMicrovmImageVersion(imageIdentifier=<that ARN>, imageVersion='1').
   AWS: ClientError with HTTP 400, Code 'ValidationException' (x-amzn-errortype 'ValidationException:http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/') and Message 'MicroVM Image not found: UpdateRuntimeStateRequest(newState=UPDATING, failureReason=null)' — the message names neither the identifier nor the version.
   Emulator: HTTP 404, Code 'ResourceNotFoundException', Message 'MicroVMImage not found for MicroVMImage: <identifier>, Version: 1'.
4. Assert all three: `assert e.response['ResponseMetadata']['HTTPStatusCode'] == 400`, `assert e.response['Error']['Code'] == 'ValidationException'`, and the message equality above.
5. Re-replay fixture lambda-microvms-image-lifecycle step task#9 to confirm the fix: the step must pass on status, $.message and $headers.x-amzn-errortype together.
