Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3.nodejs22-global-fetch-reads-presigned-object lambda__s3.nodejs22-global-fetch-tampered-presigned-url-rejected
Issue-Key: lambda__s3.nodejs22-global-fetch-reads-presigned-object|_ok#10|AssertionError|assert resp.get("FunctionError") is None, (
Issue-Key: lambda__s3.nodejs22-global-fetch-tampered-presigned-url-rejected|_ok#11|AssertionError|assert resp.get("FunctionError") is None, (
Issue-Commit: 014025136ea556b785f0af1deb0ae2c5bf2fdf97

# lambda__s3: a nodejs22.x handler's global fetch cannot reach a presigned S3 URL the emulator itself issued — every fetch faults 'TypeError: fetch failed'

> _Note: this issue was identified against the code base at commit `014025136ea556b785f0af1deb0ae2c5bf2fdf97`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause, grounded in two failing behavioral checks the replay report left unmarked (no ALREADY-FILED and no OWNED-by marker).

Cause 1 of 1: the nodejs22.x managed runtime's global fetch has no working egress path to the emulator's own S3 endpoint — an in-handler fetch of a presigned S3 GetObject URL never produces an HTTP response at all; it throws, so the invocation faults instead of returning the object (or S3's refusal).
Failing check ids: lambda__s3.nodejs22-global-fetch-reads-presigned-object, lambda__s3.nodejs22-global-fetch-tampered-presigned-url-rejected

  AWS:      Smithy (s3) models GetObject as GET /{Key+} on the bucket endpoint; the presigned form carries the credential, signature and expiry in the query string, and the model distinguishes neither virtual-hosted nor path-style addressing — both are valid. Smithy (lambda) Invoke with InvocationType RequestResponse returns StatusCode 200 and the handler's result, and sets FunctionError only when the handler itself failed. The in-function behavior is recorded behavior only: a nodejs22.x handler has a global fetch (typeof fetch === 'function'), and
              (a) fetch of an untouched presigned GetObject URL answers HTTP 200 (res.ok true) with the object's exact bytes and its stored content-type (text/plain for the recorded object); the invoke carries no FunctionError.
              (b) fetch of the same URL with one character of its X-Amz-Signature altered answers HTTP 403 whose XML body carries Code SignatureDoesNotMatch and Message "The request signature we calculated does not match the signature you provided. Check your key and signing method." — that message is entirely fixed text for this error class, interpolating nothing from the request; the invoke still carries no FunctionError, because a 403 is an HTTP response, not a handler failure.
  Emulator: neither request leaves the sandbox. Invoke answers StatusCode 200 but with FunctionError='Unhandled' and payload {"errorMessage":"fetch failed","errorType":"TypeError","trace":["TypeError: fetch failed","    at failed (node:internal/webglobals:570:35)", ... "    at raise (node:internal/https:472:27)", ...]}. The throw happens on the FIRST fetch in both checks, so the emulator serves no status, no body and no content-type on the happy path, and never reaches the 403/SignatureDoesNotMatch refusal on the tampered path.
  Why:      Fetching a presigned URL from inside a function is the documented way to give code object access without S3 credentials, and global fetch is the built-in client of the nodejs18+ runtimes. Against the emulator it fails unconditionally, and it fails as a thrown TypeError with no cause, status or code — a client cannot tell "the URL was refused" from "the object is missing" from "the network is unreachable", and its error handling for a 403 presigned refusal is never exercised. This is the emulator's own doing: it minted the presigned URL on an address its in-sandbox network layer will not route.
  Fix:      the Lambda execution environment's nodejs runtime — the global fetch/undici implementation and the outbound URL router that maps an in-function request onto a served endpoint — so a presigned URL the emulator issued is reachable from inside a function and a refusal arrives as an HTTP response (403 with S3's XML Error body) rather than a thrown TypeError. Open issue 20261115T013003Z-lambda-s3-a-presigned-s3-url-the-emulator.md names that router for node's https.get, but it carries only the replay key lambda__s3.nodejs-handler-https-get-fetches-a-presigned-url and is scoped to an ENOTFOUND refusal whose message lists the services the emulator serves; both checks here fail on global fetch with a bare "TypeError: fetch failed" and are not covered by it.
  Traces:   lambda__s3.nodejs22-global-fetch-reads-presigned-object, lambda__s3.nodejs22-global-fetch-tampered-presigned-url-rejected

## Evidence
Both failing checks [verdict: mismatch] fault on the first in-handler fetch.

Check lambda__s3.nodejs22-global-fetch-reads-presigned-object
Setup: s3:CreateBucket; s3:PutObject(Key="fetch/object.txt", Body=<text>, ContentType="text/plain"); generate a presigned GetObject URL for that object; create a nodejs22.x function (Handler index.handler) whose handler returns {fetchType, status, ok, contentType, body} from `await fetch(event.url)`; Invoke RequestResponse with {"url": <presigned url>}.
Expected (AWS): no FunctionError; fetchType "function"; status 200 and ok true; body exactly the bytes PutObject wrote; contentType "text/plain".
Actual (emulator): FunctionError='Unhandled', payload b'{"errorMessage":"fetch failed","errorType":"TypeError","trace":["TypeError: fetch failed","    at failed (node:internal/webglobals:570:35)","    at apply (native)","    at <anonymous> (node:internal/events:94:38)","    at raise (node:internal/https:472:27)","    at answer (node:internal/https:327:43)","    at send (node:internal/https:314:21)","    at <anonymous> (node:internal/https:235:14)"]}'. Replay assertion: "Invoke <fn>: FunctionError='Unhandled', payload ...".

Check lambda__s3.nodejs22-global-fetch-tampered-presigned-url-rejected
Setup: same bucket/object and presigned URL, plus a copy of the URL with the first character of its X-Amz-Signature replaced by a different hex digit; the handler fetches the good URL then the tampered one and reports {status, Code, Message} parsed out of the XML body.
Expected (AWS): no FunctionError; good.status 200 with the object's body; tampered.status 403, tampered.code "SignatureDoesNotMatch", tampered.message "The request signature we calculated does not match the signature you provided. Check your key and signing method." (entirely fixed text for this error class).
Actual (emulator): identical FunctionError='Unhandled' / {"errorMessage":"fetch failed","errorType":"TypeError", ...} payload — the control fetch already throws, so the 403 refusal side is never reached.

## Steps to Reproduce
1. git_clone() the project and boot the emulator built from the checkout.
2. s3:CreateBucket <bucket>; s3:PutObject(Bucket=<bucket>, Key="fetch/object.txt", Body=<some text>, ContentType="text/plain").
3. Generate a presigned GetObject URL for that object signed for the object's own S3 URL.
4. iam:CreateRole trusting lambda.amazonaws.com; lambda:CreateFunction(Runtime="nodejs22.x", Handler="index.handler") with index.js = `exports.handler = async (event) => { const out = { fetchType: typeof fetch }; if (typeof fetch !== 'function') return out; const res = await fetch(event.url); out.status = res.status; out.ok = res.ok; out.contentType = res.headers.get('content-type'); out.body = await res.text(); return out; };` and wait for State Active.
5. lambda:Invoke(InvocationType="RequestResponse", Payload={"url": <presigned url>}).
6. Observe FunctionError='Unhandled' with payload {"errorMessage":"fetch failed","errorType":"TypeError", ...} instead of {fetchType:"function", status:200, ok:true, contentType:"text/plain", body:<the object's bytes>}.
7. Repeat with a second URL whose first X-Amz-Signature character is swapped for a different hex digit and a handler that fetches both: AWS answers 200 for the good URL and 403 / SignatureDoesNotMatch / "The request signature we calculated does not match the signature you provided. Check your key and signing method." for the tampered one; the emulator throws on the first fetch.
8. Re-replay with: lambda__s3.nodejs22-global-fetch-reads-presigned-object, lambda__s3.nodejs22-global-fetch-tampered-presigned-url-rejected
