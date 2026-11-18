Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.code-signing-enforce-rejects-unsigned-package-rejected lambda.deep-lambda-1-fn-state-vpc-attach-in-progress-invoke-allowed-code-update-rejected lambda.deep-lambda-1-fn-state-padded-code-update-in-progress-serves-previous-code lambda.event-invoke-failure-not-surfaced
Issue-Key: lambda.code-signing-enforce-rejects-unsigned-package-rejected|_client_error#22|AssertionError|raise AssertionError(
Issue-Key: lambda.deep-lambda-1-fn-state-vpc-attach-in-progress-invoke-allowed-code-update-rejected|_client_error#8|AssertionError|raise AssertionError(f"expected a ClientError but the call succeeded and returned {result!r}")
Issue-Key: lambda.deep-lambda-1-fn-state-padded-code-update-in-progress-serves-previous-code|run#9|AssertionError|raise AssertionError(
Issue-Key: lambda.event-invoke-failure-not-surfaced|run#22|AssertionError|assert nf_msg == expected_nf_msg, (
Issue-Commit: 677b6e35c5226b67b9bfea5e554d250e3c084a9c

# lambda: code signing is enforced, an in-flight code update is refused and keeps the previous package serving, and Invoke's ResourceNotFoundException names the ARN the request addressed (4 of the parent report's 15 replay keys)

> _This report carries the causes that are FIXED. The parent report bundled eight causes; the five that are still open were re-filed as `issues/things-that-do-not-work/20261118T120000Z-lambda-cross-region-store-isolation-response-streaming-extensions-and-the-exec-wrapper.md`, so nothing still failing is recorded here as working._

## Operation
Invoke

## Description
Three root causes across the Lambda control plane's refusals and the invoke contract, all four of their replay keys now passing.

Cause 1 of 3: an unsigned package was accepted under a code-signing policy of ENFORCE.
AWS: Smithy models UpdateFunctionCode/CreateFunction errors CodeVerificationFailedException (client, HTTP 400 — "The code signature failed one or more of the validation checks for signature mismatch or expiry, and the code signing policy is set to ENFORCE. Lambda blocks the deployment.") and InvalidCodeSignatureException (client, 400), plus CodeSigningConfigNotFoundException (404). A function with an ENFORCE code-signing config must refuse an unsigned deployment package.
Emulator before the fix: the call SUCCEEDED — 200 with the updated FunctionConfiguration, no error.
Fixed by: one reading of the policy shared by both deployment operations (`crate::state::untrusted_deployment_refusal`), applied by `CreateFunction` and `UpdateFunctionCode`.
Traces: lambda.code-signing-enforce-rejects-unsigned-package-rejected.

Cause 2 of 3: there was no in-flight update window, so a concurrent code update was never refused and the new package served at once.
AWS: FunctionConfiguration carries State/LastUpdateStatus; while LastUpdateStatus=InProgress a second UpdateFunctionCode is refused with ResourceConflictException (modeled on UpdateFunctionCode, client, HTTP 409), and the previous code keeps serving invocations until the update completes.
Emulator before the fix: UpdateFunctionCode issued inside the window returned 200 instead of the 409; and across three padded updates no InProgress window stayed open across a mid-flight Invoke, because the update was applied synchronously — an invocation inside the window ran the package that had just been uploaded.
Fixed by: the function-state/LastUpdateStatus state machine and the conflict check in the UpdateFunctionCode / UpdateFunctionConfiguration handlers (first leg), and the deployment the update replaced being kept as what invocations RUN until the clock makes the update's completion due (`FunctionRecord::serving_code`, `deploy_code`, `serving_code_of`, `serving_revision_of`) while every REPORT of the function names the new package at once (second leg).
Traces: lambda.deep-lambda-1-fn-state-vpc-attach-in-progress-invoke-allowed-code-update-rejected, lambda.deep-lambda-1-fn-state-padded-code-update-in-progress-serves-previous-code.

Cause 3 of 3: Invoke's ResourceNotFoundException message appended a qualifier the caller did not send.
AWS: Invoke models ResourceNotFoundException (client, HTTP 404); for an unqualified function ARN the message is "Function not found: <the ARN as supplied>" — the fixed text is "Function not found: ", the variable part is exactly the FunctionName/ARN from the request.
Emulator before the fix: the message was "Function not found: <arn>:$LATEST" — it appended ':$LATEST' to an unqualified ARN.
Fixed by: `crate::state::addressed_arn_of` spelling the addressed resource through `qualified_arn`, the one spelling of a function at a version the rest of this service already used — so the qualifier appears only when the request named one. (`PutFunctionConcurrency` is deliberately unchanged: its own report records AWS appending ':$LATEST' there.)
Traces: lambda.event-invoke-failure-not-surfaced.

## Evidence
Expected vs actual, from the replay that filed the parent report:
- code-signing-enforce-rejects-unsigned-package-rejected: expected a refusal (CodeVerificationFailedException, 400); AssertionError: the call was expected to be refused but it SUCCEEDED — an unsigned deployment package was accepted under an Enforce code-signing policy
- deep-lambda-1-fn-state-vpc-attach-in-progress-invoke-allowed-code-update-rejected: expected ResourceConflictException (409); got 200 with FunctionConfiguration (LastUpdateStatus 'InProgress', LastUpdateStatusReasonCode 'Creating')
- deep-lambda-1-fn-state-padded-code-update-in-progress-serves-previous-code: AssertionError: Invoke while the padded update is InProgress must be served by the PREVIOUS code ('old-v1'), got 'new-v2'
- event-invoke-failure-not-surfaced: expected "Function not found: arn:aws:lambda:<region>:<acct>:function:<name>"; got "Function not found: arn:aws:lambda:<region>:<acct>:function:<name>:$LATEST"

## Steps to Reproduce
1. Code signing: CreateCodeSigningConfig with UntrustedArtifactOnDeployment='Enforce', attach it to a function, then UpdateFunctionCode with an unsigned zip -> CodeVerificationFailedException 400.
2. In-flight update: CreateFunction with a VpcConfig (or a padded code update), and while GetFunctionConfiguration reports LastUpdateStatus='InProgress' call UpdateFunctionCode -> ResourceConflictException 409; Invoke inside the same window -> served by the package that was already in place, and by the uploaded one once the update completes.
3. Messages: Invoke a non-existent function by unqualified ARN -> "Function not found: <that ARN>", with no qualifier appended.
4. Re-replay the check ids in trace_ids.
