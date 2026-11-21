Issue-Source: trace-sweep-divergence
Issue-Trace: networkflowmonitor-tag-and-untag-resource-round-trip
Issue-Step: teardown#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: networkflowmonitor-tag-and-untag-resource-round-trip
Issue-Shape-Id: com.amazonaws.networkflowmonitor#NetworkFlowMonitor
Issue-Key: networkflowmonitor|GetScope|4xx->4xx|$.message
Issue-Commit: 75d5a886d5957714d115dd7c0bb886a0e7d1137c

# networkflowmonitor splices the Java-SDK exception envelope into the wire error message of every client-error refusal

> _Note: this issue was identified against the code base at commit `75d5a886d5957714d115dd7c0bb886a0e7d1137c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScope

## Description
Cause 1 of 1: networkflowmonitor appends an SDK-client-side exception envelope to the serialized `message` member of every client-error (HTTP 400-499) response body, so the wire message is the service's own sentence plus trailing text that only an AWS SDK for Java v2 exception's getMessage() renders on the client. Replayed on GetScope against a scope ARN the emulator no longer holds (the teardown probe of the tag/untag round-trip), but the cause is in a shared response-finishing layer and therefore affects EVERY refusal this service words.

AWS: a restJson1 error body carries ONLY the service's own sentence in `message`; the error class travels in `__type` / `x-amzn-ErrorType` and the request id in the `x-amzn-RequestId` header. Smithy: GetScope binds GET /scopes/{scopeId} and models ResourceNotFoundException (httpError 404), whose only member is `message`; sibling errors are ValidationException (400), AccessDeniedException (403), ServiceQuotaExceededException (402), ThrottlingException (429), InternalServerException (500). The model does not word the sentence, so for the sentence itself this is recorded behavior only: the recorded 404 body's `message` is exactly the fixed text "There's no scope with ARN: [" + the scope ARN + "]". The fixed text is "There's no scope with ARN: [...]"; the variable part is the ARN, built from the request's region and account and the scope id addressed by the request/held in state. Nothing else belongs in the member. The clause " (Service: <sdkId>, Status Code: <n>, Request ID: <uuid>) (SDK Attempt Count: <n>)" is produced by the Java SDK's exception formatting AROUND whatever the service sent; it is never part of the serialized `message`, which is why a botocore/boto3 (or any non-Java) client sees only the sentence.

Emulator: status 404 and the error code are correct, but the message member diverges. The sentence is built with a trailing marker (crate::state::SDK_ENVELOPE_MARKER, appended by crate::state::refusal_message) and the response layer substitutes that marker with the rendered envelope for every 400-499 body, so replay observed `$.message` = "There's no scope with ARN: [<scopeArn>] (Service: NetworkFlowMonitor, Status Code: 404, Request ID: <uuid>) (SDK Attempt Count: 1)" where the fixture recorded "There's no scope with ARN: [<scopeArn>]". Because the trailer embeds the per-response request id, the message is also non-deterministic.

Why: a client that matches, asserts on, logs or surfaces the error message gets text AWS never sends, and no exact-match or equality test on the message can ever succeed because the trailer carries a fresh request id each call. A Java client is worse off still: it renders its own envelope around the emulator's, producing a doubled " (Service: ...) (SDK Attempt Count: ...) (Service: ...) (SDK Attempt Count: ...)" message. The extra bytes also inflate every error body and skew content-length comparisons.

Fix: the service's refusal wording and its response-finishing layer. The sentence builder must emit the bare sentence with no envelope marker, and the response layer must stop rewriting client-error bodies to splice SDK-side facts into `message`; the answering service, the status and the request id already travel in the status line and the `x-amzn-RequestId` header, which is where a client reads them.

Note: an OPEN behavioral issue describes the same root cause from the behavioral suite — "networkflowmonitor writes the Java-SDK client-side envelope into the wire error message..." (20261120T062610Z-networkflowmonitor-writes-the-java-sdk-client-side-envelope.md, Issue-Corpus: simparity-behavioral-aws-v1). It carries only behavioral check ids and NO wire replay key, and this divergence was reported un-owned, so this issue exists to bind the wire fixture step to the fix.

Traces: networkflowmonitor-tag-and-untag-resource-round-trip#teardown#9 (GetScope on a deleted/unknown scope -> ResourceNotFoundException message). Code citations: services/service-com-amazonaws-networkflowmonitor-networkflowmonitor/src/service/sdk_envelope.rs lines 46-111 (SDK_SERVICE_NAME, envelope, render_envelope, report) and src/state.rs (SDK_ENVELOPE_MARKER, refusal_message, scope_not_found).

## Evidence
Fixture networkflowmonitor-tag-and-untag-resource-round-trip, step teardown#9, operation GetScope:
  expected HTTP 404, actual HTTP 404 (status and error code agree)
  divergence at $.message (value_mismatch):
    expected: "There's no scope with ARN: [arn:aws:networkflowmonitor:us-west-2:000000000000:scope/00000000-0000-4000-8000-000000000001]"
    actual:   "There's no scope with ARN: [arn:aws:networkflowmonitor:us-west-2:000000000000:scope/00000000-0000-4000-8000-000000000001] (Service: NetworkFlowMonitor, Status Code: 404, Request ID: e58024e6-977f-43fd-89bd-1c08d02ccd84) (SDK Attempt Count: 1)"

The region, account, scope id and request id above are this run's values, not requirements: the requirement is that `message` is the fixed text "There's no scope with ARN: [...]" wrapping the ARN built from the request's region/account and the requested scope id, and NOTHING after the closing bracket. The divergence is the appended " (Service: ..., Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)" clause itself, which is SDK-client-side formatting and must never appear on the wire.

Source confirms the cause is service-wide: src/state.rs refusal_message() pushes SDK_ENVELOPE_MARKER onto every client-error sentence, and src/service/sdk_envelope.rs report() substitutes that marker with " (Service: NetworkFlowMonitor, Status Code: {status}, Request ID: {id}) (SDK Attempt Count: 1)" for any 400-499 response body.

## Steps to Reproduce
1. Boot the emulator and point a boto3/botocore networkflowmonitor client (or raw HTTP client) at it.
2. Run the tag/untag round-trip fixture: create a scope, tag it via POST /tags/{resourceArn}, untag it via DELETE /tags/{resourceArn}?tagKeys=..., then tear the scope down.
3. Issue GET /scopes/{scopeId} for the now-deleted scope id (fixture step teardown#9).
4. Observed: HTTP 404 with body message "There's no scope with ARN: [<scopeArn>] (Service: NetworkFlowMonitor, Status Code: 404, Request ID: <uuid>) (SDK Attempt Count: 1)".
   Expected: HTTP 404 with body message "There's no scope with ARN: [<scopeArn>]" and nothing after the closing bracket; the service name, status and request id belong to the status line and the x-amzn-RequestId header.
5. Any other refusal of this service (GetMonitor on an unknown name, DeleteScope while a monitor references the scope, a validation refusal) shows the same trailer, confirming the one shared cause.
