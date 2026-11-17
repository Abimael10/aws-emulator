Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-http-origin-header-carries-cors-allow-origin
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-http-origin-header-carries-cors-allow-origin
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|ListTables|4xx->4xx|$headers.access-control-allow-origin
Issue-Key: dynamodb|ListTables|4xx->4xx|$headers.access-control-expose-headers
Issue-Key: dynamodb|ListTables|2xx->4xx|$headers.access-control-allow-origin
Issue-Key: dynamodb|ListTables|2xx->4xx|$headers.access-control-expose-headers
Issue-Related: 20261117T221515Z-dynamodb-refusal-wording-and-validation-precedence-diverge-across.md
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# DynamoDB HTTP edge omits CORS response headers and refuses an Origin-bearing ListTables with 403 (2 causes)

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTables

## Description
Two net-new causes on dynamodb at the HTTP edge: a request carrying an Origin header is answered without the cross-origin response headers DynamoDB adds, and a second such request is refused 403 instead of being served.

Cause 1 of 2: an error response to an Origin-bearing request carries no CORS headers.
AWS: recorded behavior only — the Smithy service shape declares no @cors trait, so the recording is the specification. DynamoDB's endpoint answers a request that carries an Origin header with 'access-control-allow-origin: *' and 'access-control-expose-headers: x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date,smithy-protocol' — on error responses as well as successful ones (here alongside the HTTP 400 the malformed body earns). Both header values are fixed text, not per-request values.
Emulator: the HTTP 400 status matches, but both headers are absent.
Why: a browser-based client cannot read the response at all — without access-control-allow-origin the fetch is blocked, and without access-control-expose-headers the request id and error-type headers an SDK reads for error classification are invisible.
Fix: the service's HTTP response layer (the shared header/middleware stage that decorates every response, including error responses).
Traces: dynamodb-http-origin-header-carries-cors-allow-origin#task#1.

Cause 2 of 2: a well-formed, Origin-bearing ListTables is refused 403 instead of answered 200 with the CORS headers.
AWS: the same request with a valid signature is served: HTTP 200 with TableNames = [] (an empty list, not an omitted member) plus the same two cross-origin headers.
Emulator: HTTP 403 — no TableNames and no CORS headers.
Why: a request that differs from an accepted one only by carrying an Origin header is rejected as unauthorized, so every browser-origin call to the emulator fails authentication; the Origin header must not participate in request authorization.
Fix: the request-authorization path must exclude the Origin header from what it authenticates on, and the response layer must add the cross-origin headers to the success response.
Traces: dynamodb-http-origin-header-carries-cors-allow-origin#task#2.

## Evidence
Cause 1 — ListTables with an Origin header: expected headers access-control-allow-origin='*' and access-control-expose-headers='x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date,smithy-protocol' on the 400 response; actual both missing (status 400 matched).
Cause 2 — ListTables with an Origin header: expected 200 with TableNames=[] and the same two headers; actual 403 with TableNames and both headers missing.

## Steps to Reproduce
1. Replay fixture dynamodb-http-origin-header-carries-cors-allow-origin (task#1 and task#2) against the emulator.
2. POST to the DynamoDB endpoint with X-Amz-Target: DynamoDB_20120810.ListTables, a valid sigv4 signature and an 'Origin: https://example.com' header; observe HTTP 403 instead of 200 with TableNames=[].
3. Repeat with a malformed body: observe HTTP 400 without access-control-allow-origin / access-control-expose-headers.
