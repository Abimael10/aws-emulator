Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.url-aws-iam-signature-scoped-to-another-service-rejected lambda.url-aws-iam-signed-request-429-when-reserved-concurrency-zero lambda.url-aws-iam-tampered-body-rejected-with-invalid-signature lambda.url-aws-iam-unsigned-request-forbidden-signed-request-served-with-caller-identity lambda.url-binary-request-body-reaches-handler-base64-encoded lambda.url-cookie-header-split-into-event-cookies-array lambda.url-deleted-config-stops-serving-the-url lambda.url-form-urlencoded-body-is-base64-encoded-while-text-plain-is-not lambda.url-function-timeout-rejected-with-502 lambda.url-get-event-v2-carries-raw-path-query-and-lowercased-headers lambda.url-invalid-status-code-result-rejected-with-502 lambda.url-on-alias-qualifier-routes-to-the-alias-version-and-follows-alias-updates lambda.url-plain-string-result-served-as-bare-text-unlike-invoke lambda.url-response-base64-body-decoded-to-raw-bytes lambda.url-response-cookies-emitted-as-set-cookie-headers lambda.url-response-over-6mb-rejected-with-502 lambda.url-response-status-headers-and-body-mapped-from-handler-result lambda.url-result-without-content-type-or-body-gets-json-default-and-empty-body lambda.url-result-without-status-code-inferred-as-200-json lambda.url-text-request-body-reaches-handler-verbatim-not-base64 lambda.url-unhandled-function-error-rejected-with-502-without-leaking-the-error
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.url-aws-iam-signature-scoped-to-another-service-rejected|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: lambda.url-aws-iam-signed-request-429-when-reserved-concurrency-zero|_poll_until#5|TimeoutError|raise TimeoutError(f"{description} not observed within {timeout_s:.0f}s; last saw {last!r}")
Issue-Key: lambda.url-aws-iam-tampered-body-rejected-with-invalid-signature|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Key: lambda.url-aws-iam-unsigned-request-forbidden-signed-request-served-with-caller-identity|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: lambda.url-binary-request-body-reaches-handler-base64-encoded|run#7|AssertionError|assert resp["status"] == 200, f"{content_type}: the POST must be served, got {resp!r}"
Issue-Key: lambda.url-cookie-header-split-into-event-cookies-array|run#7|AssertionError|assert with_cookies["status"] == 200, f"the GET must be served, got {with_cookies!r}"
Issue-Key: lambda.url-deleted-config-stops-serving-the-url|run#7|AssertionError|assert served["status"] == 200, f"positive control: the URL must serve first, got {served!r}"
Issue-Key: lambda.url-form-urlencoded-body-is-base64-encoded-while-text-plain-is-not|run#8|AssertionError|assert form["status"] == 200, f"the form POST must be served, got {form!r}"
Issue-Key: lambda.url-function-timeout-rejected-with-502|run#7|AssertionError|assert (quick["status"], quick["body"]) == (200, b"quick"), f"positive control failed: {quick!r}"
Issue-Key: lambda.url-get-event-v2-carries-raw-path-query-and-lowercased-headers|run#8|AssertionError|assert resp["status"] == 200, f"the echo GET must be served 200, got {resp!r}"
Issue-Key: lambda.url-invalid-status-code-result-rejected-with-502|run#7|AssertionError|assert (valid["status"], valid["body"]) == (200, b"valid"), (
Issue-Key: lambda.url-on-alias-qualifier-routes-to-the-alias-version-and-follows-alias-updates|run#17|AssertionError|assert json.loads(alias_resp["body"]) == {"code": "one", "version": "1"}, (
Issue-Key: lambda.url-plain-string-result-served-as-bare-text-unlike-invoke|run#7|AssertionError|assert resp["status"] == 200, f"a string result is a 200, got {resp!r}"
Issue-Key: lambda.url-response-base64-body-decoded-to-raw-bytes|run#8|AssertionError|assert binary["status"] == 200, f"the GET must be served, got {binary!r}"
Issue-Key: lambda.url-response-cookies-emitted-as-set-cookie-headers|run#7|AssertionError|assert resp["status"] == 200, f"the GET must be served, got {resp!r}"
Issue-Key: lambda.url-response-over-6mb-rejected-with-502|run#7|AssertionError|assert small["status"] == 200, f"a 1 MiB body must be served, got status {small['status']!r}"
Issue-Key: lambda.url-response-status-headers-and-body-mapped-from-handler-result|run#7|AssertionError|assert created["status"] == 201, f"statusCode 201 must become the status, got {created!r}"
Issue-Key: lambda.url-result-without-content-type-or-body-gets-json-default-and-empty-body|run#7|AssertionError|assert accepted["status"] == 202, f"statusCode 202 must be the status, got {accepted!r}"
Issue-Key: lambda.url-result-without-status-code-inferred-as-200-json|run#7|AssertionError|assert resp["status"] == 200, f"{mode}: an inferred response is 200, got {resp!r}"
Issue-Key: lambda.url-text-request-body-reaches-handler-verbatim-not-base64|run#7|AssertionError|assert resp["status"] == 200, f"{content_type}: the POST must be served, got {resp!r}"
Issue-Key: lambda.url-unhandled-function-error-rejected-with-502-without-leaking-the-error|run#7|AssertionError|assert (ok["status"], ok["body"]) == (200, b"fine"), f"positive control failed: {ok!r}"
Issue-Commit: b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d

# REOPENS the function-URL data plane: an AWS_IAM Lambda function URL refuses every correctly SigV4-signed request with 403 InvalidSignatureException, so no function URL request is ever served (1 cause, 21 checks)

> _Note: this issue was identified against the code base at commit `b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
REOPENS 20261029T211043Z-lambda-data-plane-function-url-endpoints-are-unrouted.md: its fix left the call refused. That issue carries exactly these replay keys and claimed the function URL data plane would serve the handler's response; the URL host is now routed, but every request to it is refused before the function runs.

Cause 1 of 1: the function URL data plane's AWS_IAM (SigV4) authorizer rejects a correctly signed request.

AWS: a function URL config created by CreateFunctionUrlConfig is served at its own HTTPS endpoint. With AuthType=AWS_IAM the request is SigV4-signed for service name `lambda` in the function's region with the caller's credentials; AWS verifies that signature, authorizes lambda:InvokeFunctionUrl, and then invokes the function, returning the handler's mapped payload-format-2.0 result (statusCode becomes the HTTP status, headers/cookies/body are mapped, isBase64Encoded controls decoding). Only a MISSING, TAMPERED or wrongly-scoped signature is refused, and that refusal is the URL authorizer's Forbidden response (403, x-amzn-errortype naming the authorization failure), never a signature-calculation error on a request that was signed correctly. Smithy: Lambda CreateFunctionUrlConfig/GetFunctionUrlConfig return FunctionUrl and AuthType (AWS_IAM|NONE); the URL is a data-plane endpoint of the service, so the signing scope is sigv4 name `lambda` in the endpoint's region. Recorded behavior for the positive controls in these checks: HTTP 200 with the handler's body.

Emulator: every signed request to the function URL host is answered HTTP 403 with `content-type: application/json`, `x-amzn-errortype: InvalidSignatureException` and the body `{"message":"The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}`. That text is entirely fixed — it carries no request- or state-derived part — and the emulator emits it for requests whose signature the harness computed with botocore's SigV4Auth over the same credentials the emulator accepts on its control plane. The function is never invoked: the checks that assert on the handler's result see the 403 in place of the 200 positive control, and the three checks that poll for the signed request to be served instead burn their whole budget (`Check timed out after 60.0s`, and `TimeoutError: the signed GET being served not observed within 54s; last saw {'status': 403 ... 'InvalidSignatureException' ...}`).

Why: a client that deploys an AWS_IAM function URL and calls it with ordinary SDK/sigv4 credentials can never reach its own handler — every request is refused as a signature mismatch. Both halves of the contract are destroyed: the positive path (payload-format-2.0 request mapping, result mapping, alias-qualifier routing, 502 classes for malformed/oversize/erroring results, deleted-config behaviour) is unobservable, and the negative path is useless because a genuinely bad signature is indistinguishable from a good one. It also masks the real refusal shape, since AWS answers an authorization failure, not InvalidSignatureException.

Fix: the function URL data-plane listener's AWS_IAM authorization step for Lambda — the canonical request/string-to-sign construction used to verify an inbound SigV4 signature on the function URL host (host/port and path as announced to the client, service name `lambda`, region, payload hash), and the refusal shape it emits when verification genuinely fails. Related open issue 20261115T194151Z-cloudfront-lambda-2-net-new-causes-an-aws.md reports the same refusal reached through a CloudFront OAC origin under the cloudfront__lambda heading (and names the code SignatureDoesNotMatch), and 20261114T170134Z-lambda-a-function-url-authorizer-refusal-serves-the.md covers the wording of the genuine Forbidden body; neither carries the replay keys below.

Traces: lambda.url-get-event-v2-carries-raw-path-query-and-lowercased-headers and lambda.url-response-status-headers-and-body-mapped-from-handler-result show the 403 body verbatim; lambda.url-aws-iam-signed-request-429-when-reserved-concurrency-zero shows the poller's last observation; the remaining ids are in trace_ids.

## Evidence
Expected (AWS, and the positive control of every listed check): HTTP 200 and the handler's mapped response for a correctly SigV4-signed GET/POST to the function URL; a refusal only for an unsigned/tampered/wrongly-scoped signature, and then as the URL authorizer's Forbidden response.

Actual (emulator): HTTP 403, headers `{'content-type': 'application/json', 'x-amzn-errortype': 'InvalidSignatureException', ...}`, body `b'{"message":"The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}'`.

Sample assertions:
- `AssertionError: the echo GET must be served 200, got {'status': 403, ... 'x-amzn-errortype': 'InvalidSignatureException' ...}` (lambda.url-get-event-v2-carries-raw-path-query-and-lowercased-headers)
- `AssertionError: statusCode 201 must become the status, got {'status': 403, ...}` (lambda.url-response-status-headers-and-body-mapped-from-handler-result)
- `AssertionError: a 1 MiB body must be served, got status 403` (lambda.url-response-over-6mb-rejected-with-502)
- `AssertionError: the alias URL must run version 1, got b'{"message":"The request signature we calculated does not match ..."}'` (lambda.url-on-alias-qualifier-routes-to-the-alias-version-and-follows-alias-updates)
- `TimeoutError: the signed GET being served not observed within 54s; last saw {'status': 403, ... 'InvalidSignatureException' ...}` (lambda.url-aws-iam-signed-request-429-when-reserved-concurrency-zero)
- `Error: Check timed out after 60.0s` (lambda.url-aws-iam-signature-scoped-to-another-service-rejected, lambda.url-aws-iam-tampered-body-rejected-with-invalid-signature, lambda.url-aws-iam-unsigned-request-forbidden-signed-request-served-with-caller-identity)

The request id, date header and the generated function/alias names in the above are per-run values, not part of the requirement; the status, the x-amzn-errortype and the quoted message text are.

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. replay the check ids in trace_ids.
Manually: CreateFunction (python3.12) + CreateFunctionUrlConfig(AuthType='AWS_IAM'); sign a GET of the returned FunctionUrl with SigV4 (service 'lambda', the endpoint's region) using the same credentials the emulator's control plane accepts; send it. Observe 403 / x-amzn-errortype InvalidSignatureException instead of 200 with the handler's body. Repeat with AuthType='AWS_IAM' and a deliberately tampered body — the emulator's answer is indistinguishable from the correctly signed one.
