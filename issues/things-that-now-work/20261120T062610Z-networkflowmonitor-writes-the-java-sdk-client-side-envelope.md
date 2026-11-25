Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: networkflowmonitor.delete-scope-removes-scope networkflowmonitor.start-query-invalid-time-range-rejected
Issue-Shape-Id: com.amazonaws.networkflowmonitor#NetworkFlowMonitor
Issue-Key: networkflowmonitor.delete-scope-removes-scope|run#13|AssertionError|assert redelete_message == f"There's no scope with ARN: [arn:aws:networkflowmonitor:{ctx.region}:{ctx.account}:scope/{scope_id}]", (
Issue-Key: networkflowmonitor.start-query-invalid-time-range-rejected|run#12|AssertionError|assert error["Message"] == "EndTime cannot be before StartTime", (
Issue-Commit: a45c9b54f3e93c74938e99be69e1ba65771927ae

# networkflowmonitor writes the Java-SDK client-side envelope into the wire error message, so every client-error refusal's Message carries " (Service: NetworkFlowMonitor, Status Code: N, Request ID: …) (SDK Attempt Count: 1)"

> _Note: this issue was identified against the code base at commit `a45c9b54f3e93c74938e99be69e1ba65771927ae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteScope

## Description
Cause 1 of 1: the service appends an SDK-side exception envelope to the body of every client-error (HTTP 400-499) refusal, so the wire `message` member is the service sentence plus trailing text that only an AWS SDK for Java v2 exception's getMessage() renders on the client. Failing checks: networkflowmonitor.delete-scope-removes-scope, networkflowmonitor.start-query-invalid-time-range-rejected.

AWS: a restJson1 error body carries ONLY the service's own sentence in `message`, with the error class in `__type`/`x-amzn-ErrorType` and the request id in the `x-amzn-RequestId` header. Smithy: DeleteScope errors ResourceNotFoundException (HTTP 404, "The request specifies a resource that doesn't exist") and ValidationException (HTTP 400, "Invalid request"); StartQueryMonitorTopContributors errors ValidationException (HTTP 400). ResourceNotFoundException's only member is `message`. Recorded behavior: for DeleteScope on a scope id the service no longer holds, the recorded 404 message is exactly the fixed text `There's no scope with ARN: [` + the scope ARN + `]` — fixed text is "There's no scope with ARN: […]", and the variable part is the ARN built from the request's region/account and the requested scopeId. For StartQueryMonitorTopContributors with startTime after endTime, the recorded 400 message is the whole fixed sentence `EndTime cannot be before StartTime`, with no variable part. The " (Service: <sdkId>, Status Code: <n>, Request ID: <uuid>) (SDK Attempt Count: <n>)" clause is produced by the Java SDK's exception formatting around whatever the service sent; it is never part of the serialized `message`, which is why a botocore/boto3 client (and any non-Java SDK) sees only the sentence.

Emulator: the sentence is built with a trailing marker (crate::state::SDK_ENVELOPE_MARKER = "{sdk-envelope}", appended by crate::state::refusal_message) and the response layer substitutes the marker with the rendered envelope for every 400-499 body. Replay therefore observes Error.Message = "There's no scope with ARN: [arn:aws:networkflowmonitor:<region>:<account>:scope/<scopeId>] (Service: NetworkFlowMonitor, Status Code: 404, Request ID: <uuid>) (SDK Attempt Count: 1)" for the repeat DeleteScope, and "EndTime cannot be before StartTime (Service: NetworkFlowMonitor, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: 1)" for the inverted query window. The error CODE and the HTTP status are correct in both cases (ResourceNotFoundException/404, ValidationException/400); only the message text diverges. The defect is service-wide, not per-operation: every refusal worded through refusal_message (scope/monitor/query not-found, scope-in-use, quota, conflict, tagging refusals) carries the same trailer.

Why: a client that matches, asserts on, or logs the error message gets text AWS never sends — and the text is non-deterministic, because it embeds the per-response request id, so no exact-match or equality test on the message can ever succeed. A Java client is worse off still: it renders its own envelope around the emulator's, producing a doubled " (Service: …) (SDK Attempt Count: …) (Service: …) (SDK Attempt Count: …)" message. Message bytes also inflate every error body and make content-length/behavioral comparisons diverge.

Fix: the error wording and the response-finishing layer of this service. The sentence builder must emit the bare sentence (no envelope marker), and the response layer must stop rewriting client-error bodies to splice SDK-side facts into `message`; the service that answered, the status and the request id already travel in the status line and the `x-amzn-RequestId` header, which is where a client reads them.

Traces: networkflowmonitor.delete-scope-removes-scope (DeleteScope repeat -> ResourceNotFoundException message), networkflowmonitor.start-query-invalid-time-range-rejected (StartQueryMonitorTopContributors inverted window -> ValidationException message). Code citations: services/service-com-amazonaws-networkflowmonitor-networkflowmonitor/src/state.rs lines 1230-1245 (SDK_ENVELOPE_MARKER, refusal_message), lines 1305-1315 (scope_not_found), line 1515 (END_BEFORE_START); services/service-com-amazonaws-networkflowmonitor-networkflowmonitor/src/service/sdk_envelope.rs lines 46-111 (envelope/render_envelope/report).

## Evidence
Check networkflowmonitor.delete-scope-removes-scope:
  expected Error.Message == "There's no scope with ARN: [arn:aws:networkflowmonitor:<region>:<account>:scope/<scopeId>]"
  actual   Error.Message == "There's no scope with ARN: [arn:aws:networkflowmonitor:us-west-2:000000000000:scope/00000000-0000-4000-8000-000000000001] (Service: NetworkFlowMonitor, Status Code: 404, Request ID: 45da394a-47eb-4ff6-95ce-c92cd1226818) (SDK Attempt Count: 1)"
  (code ResourceNotFoundException and status 404 were correct; the scope was removed and ListScopes agreed)

Check networkflowmonitor.start-query-invalid-time-range-rejected:
  expected Error.Message == "EndTime cannot be before StartTime"
  actual   Error.Message == "EndTime cannot be before StartTime (Service: NetworkFlowMonitor, Status Code: 400, Request ID: 9cb308fe-7ffc-42f0-b7bb-bd98f7aafed9) (SDK Attempt Count: 1)"
  (code ValidationException and status 400 were correct)

The appended clause is per-response (its request id changes every call), so it is the trailer itself — not the recorded ids above — that is the divergence. Source confirms it is intentional and service-wide: state.rs refusal_message() pushes SDK_ENVELOPE_MARKER onto every client-error sentence, and service/sdk_envelope.rs report() substitutes it with " (Service: NetworkFlowMonitor, Status Code: {status}, Request ID: {id}) (SDK Attempt Count: 1)" for any 400-499 response.

## Steps to Reproduce
1. Point a boto3/botocore networkflowmonitor client at the emulator.
2. CreateScope (or reuse the account's singleton scope), wait for GetScope status SUCCEEDED, delete any monitors referencing it, then DeleteScope(scopeId=<id>).
3. Call DeleteScope(scopeId=<id>) again and read err.response["Error"]["Message"]. Observed: the "There's no scope with ARN: [...]" sentence followed by " (Service: NetworkFlowMonitor, Status Code: 404, Request ID: <uuid>) (SDK Attempt Count: 1)". Expected: the sentence alone.
4. With an ACTIVE monitor, call StartQueryMonitorTopContributors(monitorName=<name>, startTime=now, endTime=now-1h, metricName="DATA_TRANSFERRED", destinationCategory="UNCLASSIFIED") and read err.response["Error"]["Message"]. Observed: "EndTime cannot be before StartTime" plus the same trailer. Expected: "EndTime cannot be before StartTime".
5. Any other refusal of the service (e.g. DeleteScope while a monitor references the scope, GetMonitor on an unknown name) shows the same trailer, confirming one shared cause.
