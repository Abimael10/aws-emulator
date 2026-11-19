Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.xsvc-apigateway-3-http-proxy-iam-function-url-unsigned-forward-forbidden
Issue-Key: apigateway__lambda.xsvc-apigateway-3-http-proxy-iam-function-url-unsigned-forward-forbidden|_await#5|Timeout|time.sleep(interval_s if streak == 0 else 1.0)
Issue-Commit: 65202ee683bcb66d82ebd52a38b6336bbaa34d8b

# An AWS_IAM Lambda function URL fronted by a REST API HTTP_PROXY integration never answers: the whole scenario exhausts its run budget with no response served

> _Note: this issue was identified against the code base at commit `65202ee683bcb66d82ebd52a38b6336bbaa34d8b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunctionUrlConfig

## Description
One net-new root cause on the Lambda function-URL data plane as exercised through an API Gateway HTTP_PROXY integration. Numbered below with its failing check id.

Cause 1 of 1: an AWS_IAM-protected Lambda function URL serves nothing, so neither its 200 for a SigV4-signed request nor its 403 for an unsigned one can be observed, and the REST API HTTP_PROXY hop in front of it is never graded.

AWS: CreateFunctionUrlConfig (Smithy: POST /2021-10-31/functions/{FunctionName}/url, output member `FunctionUrl` is @required with @length 40-100) publishes the address at which the function is invoked over HTTP. With AuthType=AWS_IAM and a `lambda:InvokeFunctionUrl` permission for the caller's account, a SigV4-signed (service `lambda`) GET to that URL is served HTTP 200 by the function — here the echo handler answers `content-type: application/json`, header `x-echo-backend: fn-url` and a JSON body whose `path` is the request's path and whose `rawQueryString` is the request's query string. An UNSIGNED GET to the same URL is refused by the function URL itself, before the function runs, with HTTP 403 and the body exactly `{"Message": "Forbidden"}` (fixed text; `Message` capitalised, that is the function-URL refusal body, not the execute-api `{"message": ...}` form). Because an API Gateway HTTP_PROXY integration forwards the request WITHOUT signing it, a REST API stage placed in front of that AWS_IAM function URL relays exactly that 403 and that body, carries no `x-echo-backend` header, and does not echo the request. (The 403/`{"Message": "Forbidden"}` pairing and the "no signature on the proxied hop" rule are recorded behavior of this scenario.)

Emulator: nothing is served. The check does not produce a single graded response within its 60 s budget — the replay's only output is `Check timed out after 60.0s`. Hypothesis (the model and the recording do not state where it stalls): the first blocking wait in the scenario is the poll for the SigV4-signed GET straight to the published FunctionUrl to answer 200 with `x-echo-backend: fn-url` three times in a row, i.e. the positive control; everything after it (the unsigned 403 straight to the URL, and the stage's relayed 403) is therefore unreached. The function-URL endpoint either never accepts the signed request or never answers it in time.

Why: a client cannot invoke a function URL at all in this configuration, and the entire AWS_IAM authorization contract of function URLs is unobservable — an unsigned caller that AWS refuses 403 before the function runs cannot be shown to be refused, and an API Gateway HTTP_PROXY integration in front of such a URL (a common "gateway in front of a function URL" topology) cannot be exercised. A scenario that hangs instead of answering also costs every later assertion in the same check its verdict.

Fix: the lambda function-URL data plane (the listener that must answer on the host published in `CreateFunctionUrlConfig`/`GetFunctionUrlConfig`'s `FunctionUrl`), its SigV4 AWS_IAM authorization check together with the `lambda:InvokeFunctionUrl` / `lambda:InvokeFunction` permission evaluation, and the 403 `{"Message": "Forbidden"}` refusal it must render for an unsigned caller; plus the execute-api HTTP_PROXY integration path that forwards the unsigned hop and relays the backend's status and body verbatim. Related open issue that does NOT own this check: issues/things-that-do-not-work/20261109T152222Z-apigateway-lambda-function-urls-are-published-on-lambda.md — it lists this check id among its traces for a function URL published on `*.lambda-url.<region>.on.aws` and refused at egress, but the replay now reports this check under a different failure (a budget timeout, not `PublishedUrlRejected`) and does not mark it owned, so it is filed here; a fixer should confirm whether the published host now resolves and the remaining stall is in the function-URL request path itself.

Traces: apigateway__lambda.xsvc-apigateway-3-http-proxy-iam-function-url-unsigned-forward-forbidden.

## Evidence
Failing check apigateway__lambda.xsvc-apigateway-3-http-proxy-iam-function-url-unsigned-forward-forbidden, [mismatch]:

  Error: Check timed out after 60.0s

No response was graded. The scenario, in order, is: CreateRole + CreateFunction (python3.12 echo handler) -> CreateFunctionUrlConfig(AuthType="AWS_IAM") -> AddPermission(lambda:InvokeFunctionUrl, Principal=<account>, FunctionUrlAuthType=AWS_IAM) and AddPermission(lambda:InvokeFunction, InvokedViaFunctionUrl=True) -> CreateRestApi with a `{proxy+}` ANY method over an HTTP_PROXY integration whose uri is `<scheme>://<function-url-host>/{proxy}` -> CreateDeployment(stageName="test").

Expected (AWS), none of which was reached:
- signed GET `<FunctionUrl>/direct/ok?via=sigv4` -> 200, header `x-echo-backend: fn-url`, body JSON with `path` == "/direct/ok" and `rawQueryString` == "via=sigv4";
- GET `<stage>/items/abc?via=gateway` -> 403, NO `x-echo-backend` header, body exactly `{"Message": "Forbidden"}`, request not echoed;
- unsigned GET `<FunctionUrl>/items/abc?via=direct` -> 403 with the same `{"Message": "Forbidden"}` body;
- a repeat signed GET -> 200 with `x-echo-backend: fn-url` (the function still healthy).

Actual (emulator): the check's budget is exhausted before any of those four responses is produced; the first wait (the signed positive control, polled for 3 consecutive successes) is where it stalls.

## Steps to Reproduce
1. CreateRole trusting lambda.amazonaws.com; CreateFunction (runtime python3.12) with a handler that echoes `rawPath`, `rawQueryString`, headers, body and method and returns `{statusCode: 200, headers: {content-type: application/json, x-echo-backend: fn-url}, body: <json>}`; wait function_active_v2.
2. CreateFunctionUrlConfig(AuthType="AWS_IAM"); AddPermission(Action="lambda:InvokeFunctionUrl", Principal=<caller account>, FunctionUrlAuthType="AWS_IAM") and AddPermission(Action="lambda:InvokeFunction", Principal=<caller account>, InvokedViaFunctionUrl=True).
3. SigV4-sign (service `lambda`, current region) a GET to `<FunctionUrl>/direct/ok?via=sigv4` and send it. Expect HTTP 200, header `x-echo-backend: fn-url`, and a body echoing path `/direct/ok` and rawQueryString `via=sigv4`. The emulator produces no answer within the run budget.
4. Send the same GET unsigned. Expect HTTP 403 with body exactly `{"Message": "Forbidden"}`.
5. CreateRestApi with resource `{proxy+}`, ANY method (authorizationType NONE, requestParameters method.request.path.proxy), HTTP_PROXY integration uri `<scheme>://<function-url-host>/{proxy}` mapping integration.request.path.proxy, CreateDeployment(stageName="test"); GET `<stage-url>/items/abc?via=gateway`. Expect HTTP 403, no `x-echo-backend` header, body exactly `{"Message": "Forbidden"}` and the request NOT echoed (the gateway forwards the hop unsigned, so the function never runs).
