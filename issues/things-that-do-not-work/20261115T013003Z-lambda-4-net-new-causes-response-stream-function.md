Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.function-url-response-stream-mode-chunks-over-https lambda.stream-url-http-response-stream-metadata-sets-status-headers-and-cookies lambda.handler-working-until-its-remaining-time-floor-completes-before-the-kill lambda.restore-next-without-snapstart-returns-404 lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.function-url-response-stream-mode-chunks-over-https|run#18|AssertionError|assert streamed["content_length"] is None, (
Issue-Key: lambda.stream-url-http-response-stream-metadata-sets-status-headers-and-cookies|run#13|AssertionError|assert "content-length" not in shaped["headers"], shaped["headers"]
Issue-Key: lambda.handler-working-until-its-remaining-time-floor-completes-before-the-kill|run#17|AssertionError|assert "Status: timeout" in _report_line(over_tail, over_rid), (
Issue-Key: lambda.restore-next-without-snapstart-returns-404|run#15|AssertionError|assert control["status"] == 403, (
Issue-Key: lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies|run#8|AssertionError|assert obs["status"] == status, f"{key}: expected HTTP {status}, got {obs!r}"
Issue-Commit: dda73b1c89ff90e3a0e7799df332912882e239a1

# lambda: 4 net-new causes — RESPONSE_STREAM function URLs answer buffered with Content-Length, a timed-out invoke's REPORT omits Status: timeout, and the runtime API's restore/next and Telemetry Subscribe routes are absent (404)

> _Note: this issue was identified against the code base at commit `dda73b1c89ff90e3a0e7799df332912882e239a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeWithResponseStream

## Description
4 net-new root causes in the lambda service. All four must be closed.

Cause 1 of 4: a function URL in RESPONSE_STREAM invoke mode answers buffered, announcing Content-Length.
AWS: recorded behavior only (the function-URL data plane is not in the Smithy model). A function URL whose InvokeMode is RESPONSE_STREAM cannot know the body length before the handler finishes, so AWS answers with chunked transfer framing and NO Content-Length response header. The same holds for the metadata-prelude form (awslambda.streamifyResponse with an HttpResponseStream metadata object): the declared statusCode, headers and Set-Cookie are applied, and still no Content-Length.
Emulator: the URL answers with Content-Length set to the full body length — the response is buffered and its length announced. Observed headers on the metadata case include 'content-length': '25' alongside the correctly applied 'content-type', 'set-cookie' and custom header; the plain streaming case answered Content-Length '32'. (The byte counts are per-run values; the defect is that the header is present at all.)
Why: no client can observe streaming — a reader that expects the first chunk before the handler finishes gets one buffered body, and anything keying on chunked framing or the absence of Content-Length (the documented way to tell a streaming URL from a buffered one) is wrong.
Fix: the lambda function-URL data-plane response serializer for InvokeMode RESPONSE_STREAM.
Failing check ids for this cause: lambda.function-url-response-stream-mode-chunks-over-https; lambda.stream-url-http-response-stream-metadata-sets-status-headers-and-cookies.

Cause 2 of 4: a timed-out invocation is faked inside the handler, so its REPORT record has no Status field.
AWS: recorded behavior only. When an invocation runs past its configured timeout the platform kills the execution environment: the log carries the platform's task-timeout error line (not a handler traceback), and the REPORT record carries a 'Status: timeout' field alongside Duration, Billed Duration, Memory Size and Max Memory Used. The fixed text is the field names and the literal 'Status: timeout'; the durations and memory figures are per-run values.
Emulator: the REPORT line is 'REPORT RequestId: <id>\tDuration: <n> ms\tBilled Duration: <n> ms\tMemory Size: 128 MB\tMax Memory Used: 1 MB' with no Status field at all, and the failure is surfaced as a handler-level '[ERROR] RuntimeError: the invocation ran past its timeout' traceback whose innermost frame is '<lambda-runtime>', line 302, in _bounded_sleep. The runtime raises inside the user handler instead of the platform killing the invoke.
Why: a client cannot tell a timeout from a handler exception — the error type is a user RuntimeError and the REPORT has no Status — so timeout alarms, log-metric filters on the timeout status and any retry decision keyed on it never fire; and a handler's own exception handler can swallow a "timeout" that AWS would never let it see.
Fix: the lambda invoke timeout enforcement (kill the environment rather than raising in the handler) and the REPORT log-record serializer.
Failing check ids for this cause: lambda.handler-working-until-its-remaining-time-floor-completes-before-the-kill.

Cause 3 of 4: the Runtime API restore endpoint is not served — GET /runtime/restore/next answers 404.
AWS: recorded behavior only (the Runtime API is not in the Smithy model). On a SnapStart-enabled version the Lambda Runtime API serves GET /2018-06-01/runtime/restore/next; once the runtime has already taken its restore turn for the environment, a further GET is refused with HTTP 403. A 404 means the route does not exist at all.
Emulator: every call answers {'status': 404, 'body': ''} — the route is absent, so the SnapStart restore phase of the runtime contract cannot be exercised or refused.
Why: a custom or managed runtime implementing the documented SnapStart restore handshake gets "no such route" where AWS gives it either a restore event or a 403, so it cannot distinguish "not my turn" from "this platform has no restore phase".
Fix: the lambda runtime-API router's /runtime/restore/next route and the per-environment restore state machine behind it.
Failing check ids for this cause: lambda.restore-next-without-snapstart-returns-404.

Cause 4 of 4: the Telemetry API Subscribe endpoint is not served — every probe answers 404 with an empty body.
AWS: recorded behavior only. An extension registered during Init may PUT the Telemetry API Subscribe endpoint (/2022-07-01/telemetry); an invalid request body is refused with HTTP 400 and a JSON error body, one documented body per invalid class (unsupported schemaVersion, bad destination, bad buffering, etc.), and a valid body is accepted with HTTP 200.
Emulator: every probe — valid and invalid alike — answers {'status': 404, 'body': '', 'headers': {}}. The route is absent, so no validation class is observable; the first failing probe is the unsupported-schemaVersion case, where 400 was expected and 404 came back.
Why: an extension that subscribes to telemetry gets 404 and cannot subscribe at all, and nothing distinguishes a rejected subscription from an unimplemented API, so extension code branches on the wrong signal.
Fix: the lambda extensions/telemetry runtime-API router (the Subscribe route) and the Subscribe request validator and its error bodies.
Failing check ids for this cause: lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies.

## Evidence
Cause 1 — lambda.function-url-response-stream-mode-chunks-over-https [mismatch]: 'a RESPONSE_STREAM URL cannot know the body length up front, yet it answered with Content-Length: \'32\''. lambda.stream-url-http-response-stream-metadata-sets-status-headers-and-cookies [mismatch]: assertion "'content-length' not in shaped['headers']" failed; headers were {'content-type': 'text/plain', 'x-sim2real-stream': 'yes', 'set-cookie': 'flavor=oat', 'x-amzn-requestid': <id>, 'content-length': '25', 'connection': 'close', 'date': <ts>}.

Cause 2 — lambda.handler-working-until-its-remaining-time-floor-completes-before-the-kill [mismatch]: 'the overrun REPORT line lacks \'Status: timeout\'': log was 'START RequestId: <id> Version: $LATEST\n[ERROR] RuntimeError: the invocation ran past its timeout\nTraceback (most recent call last):\n  File "index.py", line 8, in handler\n    time.sleep(0.02)\n  File "<lambda-runtime>", line 302, in _bounded_sleep\nEND RequestId: <id>\nREPORT RequestId: <id>\tDuration: 3034.19 ms\tBilled Duration: 3035 ms\tMemory Size: 128 MB\tMax Memory Used: 1 MB'.

Cause 3 — lambda.restore-next-without-snapstart-returns-404 [mismatch]: 'GET /runtime/restore/next on a SnapStart version, after the runtime's own restore call, must be a 403, got {\'status\': 404, \'body\': \'\'}'.

Cause 4 — lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies [mismatch]: 'invalid_schema_version: expected HTTP 400, got {\'status\': 404, \'body\': \'\', \'headers\': {}}'.

## Steps to Reproduce
Cause 1:
1. Create a function with a streaming handler and a function URL with InvokeMode RESPONSE_STREAM (nodejs22.x, awslambda.streamifyResponse; one variant writes an HttpResponseStream metadata prelude with statusCode, headers and cookies).
2. GET the function URL over HTTP and inspect the response headers.
3. Observe: Content-Length is present (and the body is buffered). AWS sends no Content-Length.

Cause 2:
1. Create a python function with Timeout = 3 whose handler loops until its remaining time falls below a small floor, then keeps working past the deadline.
2. Invoke it with LogType Tail and decode the log tail.
3. Observe: the REPORT line has no 'Status: timeout' field and the log shows a handler RuntimeError traceback from <lambda-runtime> _bounded_sleep.

Cause 3:
1. Create a python3.13 function, enable SnapStart and publish a version.
2. From inside the execution environment, GET $AWS_LAMBDA_RUNTIME_API/2018-06-01/runtime/restore/next after the runtime's own restore call.
3. Observe: HTTP 404 with an empty body instead of HTTP 403.

Cause 4:
1. Create a python3.13 function whose handler registers an internal extension during Init.
2. From the extension, PUT the Telemetry API Subscribe endpoint with an unsupported schemaVersion (and with the other invalid bodies, and a valid one).
3. Observe: every call answers HTTP 404 with an empty body instead of 400 (invalid) / 200 (valid).
