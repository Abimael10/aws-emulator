Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__lambda.metrics-lambda-function-url-invoke-meters-the-same-as-the-invoke-api
Issue-Key: cloudwatch__lambda.metrics-lambda-function-url-invoke-meters-the-same-as-the-invoke-api|_poll_until#9|AssertionError|raise AssertionError(f"{description} not observed within {timeout_s}s; last saw {last!r}")
Issue-Commit: b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d

# A signed AWS_IAM function URL invoke is refused 403 InvalidSignatureException, so no AWS/Lambda metric series can be compared between the URL and the Invoke API (1 cause)

> _Note: this issue was identified against the code base at commit `b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
REOPENS 20261029T211043Z-lambda-data-plane-function-url-endpoints-are-unrouted.md: its fix left the call refused.

Cause 1 of 1: the AWS_IAM function URL authorizer refuses a correctly SigV4-signed request, so the URL-driven invocation never happens and the metrics it should publish are never emitted.

AWS: an invocation delivered through a function URL is metered exactly like one delivered through the Invoke API — the same AWS/Lambda Invocations/Duration/Errors series under the FunctionName dimension. Reaching the URL requires only a valid SigV4 signature for service `lambda` in the function's region; AWS then invokes the function and answers with the handler's mapped result. Smithy: CreateFunctionUrlConfig returns FunctionUrl with AuthType AWS_IAM|NONE; the URL is a Lambda data-plane endpoint, so the signing scope is sigv4 name `lambda`. Recorded behavior: the signed GET is served 200 and the metric datapoint follows.

Emulator: the signed GET is answered HTTP 403 with `x-amzn-errortype: InvalidSignatureException` and the fixed body `{"message":"The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}`, for a request signed by botocore's SigV4Auth with the same credentials the emulator's control plane accepts. The function is never invoked, so the check never gets past its positive control and is killed polling for it.

Why: a client that fronts a function with an AWS_IAM URL sees no invocation, no handler response and no AWS/Lambda metric for URL traffic at all — URL-delivered load is invisible to alarming and dashboards that were built against the Invoke API's series.

Fix: the function URL data-plane listener's AWS_IAM SigV4 verification (canonical request/string-to-sign built over the host, port and path as announced to the client, service name `lambda`, region, payload hash) and, once requests are served, the metric emission path that must treat a URL-delivered invocation as an invocation. Related open issue 20261111T064629Z-cloudwatch-lambda-an-unsigned-aws-iam-lambda-function-url-request.md covers the body served for an UNSIGNED request; it does not cover a correctly signed one being refused and does not carry this replay key.

Traces: cloudwatch__lambda.metrics-lambda-function-url-invoke-meters-the-same-as-the-invoke-api.

## Evidence
Expected: the signed Function URL GET is served (HTTP 200 with the handler's body), after which AWS/Lambda Invocations for FunctionName=<fn> records the URL-delivered invocation alongside the Invoke-API one.

Actual: `AssertionError: a signed Function URL request being served not observed within 120.0s; last saw {'status': 403, 'body': b'{"message":"The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}'}`

The 120 s budget and the generated function name are per-run; the 403 status, the InvalidSignatureException classification and the quoted message are the divergence.

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. replay cloudwatch__lambda.metrics-lambda-function-url-invoke-meters-the-same-as-the-invoke-api.
Manually: create a function, CreateFunctionUrlConfig(AuthType='AWS_IAM'), SigV4-sign a GET of the returned URL (service 'lambda', endpoint region) and send it; observe 403 InvalidSignatureException. Then GetMetricData for AWS/Lambda Invocations, FunctionName=<fn> — no datapoint for the URL request.
