Issue-Source: trace-sweep-divergence
Issue-Trace: dsql-cdc-stream-error-handling
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dsql-cdc-stream-error-handling
Issue-Shape-Id: com.amazonaws.dsql#DSQL
Issue-Key: dsql|GetStream|4xx->4xx|$.message
Issue-Key: dsql|GetStream|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: dsql|GetStream|4xx->4xx|status
Issue-Key: dsql|DeleteStream|4xx->4xx|$.message
Issue-Key: dsql|DeleteStream|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: dsql|DeleteStream|4xx->4xx|status
Issue-Commit: 2986c932db5644b5e53751894a3faacc2d59c288

# dsql stream reads skip streamIdentifier format validation, answering 404 ResourceNotFoundException where AWS answers 400 ValidationException

> _Note: this issue was identified against the code base at commit `2986c932db5644b5e53751894a3faacc2d59c288`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStream

## Description
The two stream-addressing operations that take a `streamIdentifier` path label never validate that the identifier is a well-formed DSQL stream id. They go straight to the store lookup, so a syntactically invalid identifier is reported as a resource that does not exist instead of as a malformed request. AWS validates the identifier's FORM before it ever looks for the resource, so the two outcomes are distinguishable on the wire by status, error code and message; the emulator collapses them into the not-found path.

Note on scope: the shared Smithy pattern `^[a-z0-9]{26}$` is NOT the whole rule. A DSQL stream id is a base32-encoded 26-character string, whose alphabet is a strict subset of `[a-z0-9]`, so an input can satisfy the modeled pattern and still be refused by the service. The validation the emulator is missing is this stricter base32 check, which must run before the store lookup.

Cause 1 of 2: GetStream (fixture `dsql-cdc-stream-error-handling`, step `task#5`).
AWS: Smithy `GetStream` — `GET /stream/{clusterIdentifier}/{streamIdentifier}`, input member `streamIdentifier` target `StreamId` @pattern `^[a-z0-9]{26}$`, @required, @httpLabel; modeled errors include `ValidationException` (httpError 400) and `ResourceNotFoundException` (httpError 404). The modeled pattern does not express the base32 alphabet, so for the alphabet rule this is recorded behavior only: the recording shows AWS answering HTTP 400 with `x-amzn-errortype` naming the validation error and the body message "Aurora DSQL can't process the request because the streamIdentifier that you specified isn't valid. Provide the streamIdentifier in the format 'a valid base32-encoded 26-character string' and try again." In that message the fixed text is everything except the member name: "Aurora DSQL can't process the request because the " + <member> + " that you specified isn't valid. Provide the " + <member> + " in the format 'a valid base32-encoded 26-character string' and try again." — the only variable part is the offending input member's name (`streamIdentifier`), taken from the request; no identifier value, ARN, region or account number appears in it.
Emulator: answers HTTP 404 with `x-amzn-errortype: ResourceNotFoundException` and the body message "The resource with ARN <stream-arn> doesn't exist. Verify the ARN and try again.", echoing the malformed identifier back inside a synthesized stream ARN.
Why: a client that distinguishes "you sent me garbage" from "it isn't there" gets the wrong answer. A 400 ValidationException is a non-retryable programming error the caller must fix, while a 404 ResourceNotFoundException is routinely treated as "not created yet" and polled, or as a benign absence, so a caller with a typo'd or truncated stream id silently loops or proceeds as if the stream were simply absent. Worse, the emulator's 404 message asserts the existence of a stream ARN that could never be minted, since the malformed identifier cannot appear in a valid `StreamArn` (whose own pattern is `.../stream/[a-z0-9]{26}$` over base32 ids).
Fix: the GetStream handler's input validation — reject a `streamIdentifier` that is not a valid base32 26-character id with the modeled `ValidationException` (400) and the message template above, BEFORE the cluster/stream store lookup runs.
Traces: `dsql-cdc-stream-error-handling#task#5`.

Cause 2 of 2: DeleteStream (fixture `dsql-cdc-stream-error-handling`, step `task#6`).
AWS: Smithy `DeleteStream` — `DELETE /stream/{clusterIdentifier}/{streamIdentifier}`, same `streamIdentifier` member (`StreamId`, @pattern `^[a-z0-9]{26}$`, @required, @httpLabel); modeled errors include `ValidationException` (httpError 400), `ResourceNotFoundException` (404) and `ConflictException` (409). Recorded behavior only for the alphabet rule: AWS answers HTTP 400 with the identical validation message and template as Cause 1, with `streamIdentifier` as the member-name substitution. The identifier's form is checked before any lookup, so the mutating call never reaches the store.
Emulator: answers HTTP 404 with `x-amzn-errortype: ResourceNotFoundException` and "The resource with ARN <stream-arn> doesn't exist. Verify the ARN and try again." — the same not-found path as Cause 1, reached because no form check precedes it.
Why: delete-if-exists callers swallow 404 as success, so a caller that deletes the wrong (malformed) identifier is told the delete is done rather than that its request was invalid, and the real stream it meant to delete is left behind.
Fix: the DeleteStream handler's input validation — same base32 `streamIdentifier` check, raising the modeled `ValidationException` (400) ahead of the store lookup. The check belongs in shared input validation for the `StreamId` label so both operations (and any other operation taking a `streamIdentifier`) share one implementation.
Traces: `dsql-cdc-stream-error-handling#task#6`.

Related but NOT covering this: open issue `20261028T121757Z-dsql-getstream-error-response.md` concerns the Region segment of the ARN inside the not-found message on these same two handlers. It is about the content of the 404 for a well-formed but absent stream; it does not add the missing form validation, so these two steps still diverge after it is fixed.

## Evidence
Replay of fixture `dsql-cdc-stream-error-handling`, trace set `simparity-wire-aws-v1`:

Step `task#5` (GetStream, streamIdentifier a 26-char non-base32 string):
  expected status 400, actual status 404
  $.message value_mismatch — expected "Aurora DSQL can't process the request because the streamIdentifier that you specified isn't valid. Provide the streamIdentifier in the format 'a valid base32-encoded 26-character string' and try again."; got "The resource with ARN arn:aws:dsql:<region>:<account>:cluster/<cluster>/stream/<malformed-id> doesn't exist. Verify the ARN and try again."
  $headers.x-amzn-errortype shape_mismatch — expected the modeled validation error type; got "ResourceNotFoundException"

Step `task#6` (DeleteStream, same identifier):
  expected status 400, actual status 404
  $.message value_mismatch — expected the same validation message; got the same "The resource with ARN ... doesn't exist. Verify the ARN and try again."
  $headers.x-amzn-errortype shape_mismatch — expected the modeled validation error type; got "ResourceNotFoundException"

Model basis (services/service-com-amazonaws-dsql-dsql/smithy-model/com-amazonaws-dsql-dsql.json): GetStream and DeleteStream both bind `streamIdentifier` -> `StreamId` (@pattern ^[a-z0-9]{26}$, @required, @httpLabel) and both model `ValidationException` (httpError 400) alongside `ResourceNotFoundException` (httpError 404). `StreamArn` is patterned `^arn:aws(-[^:]+)?:dsql:[a-z0-9-]{1,20}:[0-9]{12}:cluster/[a-z0-9]{26}/stream/[a-z0-9]{26}$`, so the ARN the emulator reports in its 404 is not a constructible stream ARN for a non-base32 identifier.

Region, account, cluster id and the concrete identifier above are per-environment values from the recording; the requirement is the status (400), the error type (ValidationException) and the message template, not those values.

## Steps to Reproduce
1. Boot the emulator and create a DSQL cluster through the dsql endpoint; wait until it reports ACTIVE. Note its clusterIdentifier.
2. Call GetStream on that cluster with a streamIdentifier that is 26 characters of [a-z0-9] but is NOT valid base32 for a DSQL id (it contains letters outside the base32 alphabet, e.g. i/l/o/u): GET /stream/<clusterIdentifier>/<26-char-non-base32-id>.
3. Observe HTTP 404 with x-amzn-errortype: ResourceNotFoundException and body message "The resource with ARN arn:aws:dsql:<region>:<account>:cluster/<clusterIdentifier>/stream/<id> doesn't exist. Verify the ARN and try again." AWS answers HTTP 400 with the ValidationException error type and "Aurora DSQL can't process the request because the streamIdentifier that you specified isn't valid. Provide the streamIdentifier in the format 'a valid base32-encoded 26-character string' and try again."
4. Repeat with DELETE /stream/<clusterIdentifier>/<same id> for DeleteStream and observe the identical 404 ResourceNotFoundException instead of the 400 ValidationException.
5. Confirm the check is ordered before the lookup: the same malformed identifier against a clusterIdentifier that does NOT exist must still produce the 400 validation refusal, not a cluster not-found.
6. Replay fixture dsql-cdc-stream-error-handling steps task#5 and task#6 to confirm both clear.
