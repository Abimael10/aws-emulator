Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-web.endpoint-latest-revision-mode-rejects-switching-onto-a-non-active-revision
Issue-Shape-Id: com.amazonaws.lambdaweb#LambdaWeb
Issue-Key: lambda-web.endpoint-latest-revision-mode-rejects-switching-onto-a-non-active-revision|run#27|AssertionError|assert message == expected_message, f"unexpected conflict message {message!r}"
Issue-Commit: 67433b0ed426f9978a978ae5edd3c4e66dd34781

# lambda-web: UpdateWebFunctionEndpoint's LatestRevision switch conflict serves a generic "Resolve the conflict and try again" sentence instead of AWS's latest-revision-not-deployable message (1 cause)

> _Note: this issue was identified against the code base at commit `67433b0ed426f9978a978ae5edd3c4e66dd34781`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateWebFunctionEndpoint

## Description
ONE net-new root cause in the lambda-web emulator's endpoint auto-deployment conflict contract, reached by the replayed behavioral suite.

--------------------------------------------------------------------
Cause 1 of 1: switching an endpoint's autoDeploymentMode to LatestRevision while the web function's NEWEST revision is not Active is correctly refused with HTTP 409 / ConflictException, but the emulator reports a generic, self-authored sentence built from the revision id, the function name and the revision's state, instead of the sentence AWS words for this refusal class. The emulator's message says what resource is in what state; AWS's says that the LATEST revision cannot be DEPLOYED and what the caller must do about it (create a new revision). Failing check: lambda-web.endpoint-latest-revision-mode-rejects-switching-onto-a-non-active-revision.

AWS: Smithy (services/service-com-amazonaws-lambdaweb-lambdaweb/smithy-model/com-amazonaws-lambdaweb-lambdaweb.json), service LambdaWeb, restJson1. smithy_query(query="operation", operation="UpdateWebFunctionEndpoint") gives PATCH /2025-03-07/web-functions/{functionName}/endpoints/{endpointName}, success 202, @idempotent; input members include autoDeploymentMode (AutoDeploymentMode enum: Disabled | LatestRevision); modelled errors include ConflictException (client, httpError 409). The operation's own documentation for autoDeploymentMode states "LatestRevision - The endpoint automatically serves the newest revision", and the model's ConflictException doc covers the class only in general terms ("The request conflicts with the current state of the resource. ... Resolve the conflict and try again."). The model does NOT fix the per-operation message text, so the exact sentence is recorded behavior only. The recording shows AWS answering HTTP 409, error code ConflictException, with message:

  "The latest revision <revisionId> is in <State> state and cannot be deployed. Only revisions in Active state can be deployed. Create a new revision."

Fixed text: "The latest revision " / " is in " / " state and cannot be deployed. Only revisions in Active state can be deployed. Create a new revision." Variable parts, taken from the emulator's own state: <revisionId> — the revisionId of the function's newest revision; <State> — that revision's state word, recorded as "Failed". NOTE the function name does NOT appear in AWS's sentence at all. The refusal is total: the endpoint keeps autoDeploymentMode Disabled (the check's follow-up GetWebFunctionEndpoint asserts exactly that).

Emulator: status 409 and error code ConflictException are both correct, but the message is "The revision <revisionId> of web function <functionName> is in the <State> state. Resolve the conflict and try again." The replay reports verbatim: AssertionError: unexpected conflict message 'The revision 00000000-0000-0000-0000-000000000002 of web function sim2real-behav-lw-fn-f803fd8706a8 is in the Failed state. Resolve the conflict and try again.' (the revision id and function name shown are this run's generated values and are not part of the requirement; the divergence is the SENTENCE). The emulator both uses the wrong fixed text and injects a value AWS's message does not carry (the function name), while omitting AWS's statement of the rule ("Only revisions in Active state can be deployed. Create a new revision.").

Why: a client or IaC provider that matches AWS's documented wording to distinguish "the newest revision is not deployable — build a new one" from the other conflicts this one modelled error shape carries (an endpoint being modified by another request, a revision still serving traffic, a stale policy revision id) sees a sentence it does not recognise, and cannot tell the auto-deployment refusal apart from any other 409 of this service by text. Golden-output assertions and message-matching remediation logic diverge on the emulator only, even though status and code are right. This is the same defect family as open issue 20261109T065107Z-lambda-web-deleteresourcepolicy-stale-revision-conflictexception-serves-putresourcepolicy.md (one shared conflict sentence serving refusals AWS words distinctly); that issue is about DeleteResourcePolicy and does not cover this operation.

Fix: the message belongs to the endpoint slice of this service's state — the single sentence the latest-revision switch check returns — not to a shared ConflictException constant. That helper must word the refusal the way AWS does for THIS class: name the latest revision and its state, say it cannot be deployed, state that only Active revisions can be deployed, and tell the caller to create a new revision; drop the function name and the generic closing sentence. Keep the 409 status, the ConflictException classification, the Active-only rule and the leave-the-endpoint-unchanged semantics, which are all correct.

--------------------------------------------------------------------
Traces (failing check ids, as examples; file/line citations are against this checkout and may have drifted):
- Cause 1: lambda-web.endpoint-latest-revision-mode-rejects-switching-onto-a-non-active-revision — services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:427-439 (latest_revision_switch_conflict, whose doc at :423-426 explicitly chooses the generic "Resolve the conflict and try again." sentence), read by services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/update_web_function_endpoint.rs:350-354 (the switches_to_latest_revision branch, wrapped by conflict() at :215-222).

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 17/18 checks passed; the single failure is lambda-web.endpoint-latest-revision-mode-rejects-switching-onto-a-non-active-revision (verdict: mismatch). No check was blocked by a scaffold stub, and the failure is not marked ALREADY FILED or OWNED by an open issue.

Cause 1 — UpdateWebFunctionEndpoint(autoDeploymentMode=LatestRevision) on an endpoint currently Disabled, while the function's newest revision is Failed (its build read an object that is not a zip archive):
  Expected (AWS): HTTP 409, error code ConflictException, message "The latest revision <revisionId> is in Failed state and cannot be deployed. Only revisions in Active state can be deployed. Create a new revision." — fixed text as quoted, with <revisionId> the newest revision's id from state and the state word that revision's state. No function name appears.
  Actual (emulator): HTTP 409, ConflictException (both correct), message "The revision <revisionId> of web function <functionName> is in the Failed state. Resolve the conflict and try again."
  Replay detail (verbatim): File "check_lambda_web_endpoint_latest_revision_mode_rejects_switching_onto_a_non_active_revision.py", line 284, in run / assert message == expected_message, f"unexpected conflict message {message!r}" / AssertionError: unexpected conflict message 'The revision 00000000-0000-0000-0000-000000000002 of web function sim2real-behav-lw-fn-f803fd8706a8 is in the Failed state. Resolve the conflict and try again.'
  The check reached this assertion only after its earlier assertions passed, so the emulator correctly: built revision A to Active, built revision B to Failed with stateReason "Failed to read zip file: zip: not a valid zip file", settled the endpoint Active, and classified the refusal as ConflictException. The ONLY divergence observed is the message text.
  Source evidence: services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:427-439 formats exactly the diverging sentence ("The revision {} of web function {} is in the {} state. Resolve the conflict and try again."), and src/ops/update_web_function_endpoint.rs:350-354 returns it as the 409 body's message.
  Model evidence: smithy_query(query="operation", operation="UpdateWebFunctionEndpoint") — ConflictException at httpError 409, success 202, autoDeploymentMode enum Disabled|LatestRevision; no modelled message text, so the sentence is recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a botocore 'lambda-web' client at the data listener (any region, any credentials); also create an iam client and an s3 client against the same emulator.
2. Create an execution role trusting lambda.amazonaws.com (iam.create_role) and a versioned bucket (s3.create_bucket + put_bucket_versioning Enabled).
3. Upload a VALID zip at key code-a.zip (an index.js HTTP server is fine).
4. CreateWebFunction(functionName=<fn>, revisionConfig={buildConfig:{codeConfig:{s3Object:{bucket,key:'code-a.zip'}}, runtimeConfig:{runtime:'nodejs24.x'}}, serviceConfig:{executionRoleArn:<role>}}, endpointConfig={endpointName:'ep', endpointType:'HomeRegion', authType:'ApplicationManaged', autoDeploymentMode:'Disabled'}). Poll GetWebFunctionRevision until revision A is Active and GetWebFunctionEndpoint until 'ep' is Active.
5. Upload a body that is NOT a zip archive at key code-b.zip (e.g. b'not a zip archive'), then CreateWebFunctionRevision(functionName=<fn>, buildConfig={codeConfig:{s3Object:{bucket,key:'code-b.zip'}}, runtimeConfig:{runtime:'nodejs24.x'}}, serviceConfig={executionRoleArn:<role>}). Poll GetWebFunctionRevision until revision B reports state Failed.
6. Call UpdateWebFunctionEndpoint(functionName=<fn>, endpointName='ep', autoDeploymentMode='LatestRevision'). The emulator correctly refuses with HTTP 409 / ConflictException and leaves autoDeploymentMode='Disabled', but Error.Message is "The revision <revB> of web function <fn> is in the Failed state. Resolve the conflict and try again." instead of AWS's "The latest revision <revB> is in Failed state and cannot be deployed. Only revisions in Active state can be deployed. Create a new revision."
7. Positive control (already passing behaviour to preserve): DeleteWebFunctionRevision(revisionId=<revB>), then repeat step 6 — the switch must succeed and settle updateStatus='Successful' with autoDeploymentMode='LatestRevision'.
8. Re-replay check lambda-web.endpoint-latest-revision-mode-rejects-switching-onto-a-non-active-revision to confirm the fix.
