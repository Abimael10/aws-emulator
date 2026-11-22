Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore|_http#24|Timeout|with urllib.request.urlopen(req, timeout=timeout_s) as resp:
Issue-Commit: 8e4854a18a70a7338c2bf03aa195a8119bab6faa

# apigatewayv2: an HTTP API with DisableExecuteApiEndpoint=true never answers the documented 404 {"message":"Not Found"} on its default execute-api URL, and the disable/restore transition reaches no verdict (1 cause)

> _Note: this issue was identified against the code base at commit `8e4854a18a70a7338c2bf03aa195a8119bab6faa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApi

## Description
One net-new root cause in the apigatewayv2 execute-api front door. Cause 1 of 1: an HTTP API whose default execute-api endpoint has been disabled with UpdateApi(DisableExecuteApiEndpoint=true) never answers the documented refusal on that URL, so neither the disabled state nor the restore back to serving can be observed. Failing check: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.

AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2): UpdateApi (PATCH /v2/apis/{ApiId}, 200) takes DisableExecuteApiEndpoint (__boolean, jsonName disableExecuteApiEndpoint) and echoes it in UpdateApiResponse; GetApi (GET /v2/apis/{ApiId}, 200) reports the same member, so the flag is per-API state readable after the update. UpdateApi's modelled errors are BadRequestException 400, ConflictException 409, NotFoundException 404, TooManyRequestsException 429 — none of them applies here, the control-plane call succeeds. The data-plane consequence is recorded behavior only (the execute-api data plane is not modelled in Smithy): once the flag is set, a request to that API's default execute-api URL (https://<api-id>.execute-api.<region>.amazonaws.com/<stage>/<path>) is refused with HTTP 404 and the gateway envelope body {"message":"Not Found"} — that body is entirely fixed text, it carries no value from the request or from state — and the integration is NOT invoked, so nothing from the refused request reaches the backend (no log line, no backend body in the response). The refusal is scoped to the one API: a twin API on the same backend with the flag clear keeps answering 200 with the backend's body in the same instant. Clearing the member (DisableExecuteApiEndpoint=false) puts the same URL back to 200 from the integration.

Emulator: the check reaches no verdict — the replay detail is 'Error: Check timed out after 60.0s'. The check's first data-plane beat after the update polls the target URL for a settled run of 404s (and after re-enabling, for a settled run of 200s) and neither run ever materialises inside the harness budget, so the whole disable/restore transition is ungradeable. Hypothesis: the front door's disabled-endpoint gate answers a different refusal class than 404 {"message":"Not Found"} — the open issue 20261112T190000Z-apigatewayv2-the-disableexecuteapiendpoint-check-still-hangs-on-the-front-door-upgrade.md records this gate refusing with 403 {"message":"Forbidden"} — so the status the poll waits for never appears; the model and the recording state only that the documented refusal is 404 with that envelope.

Why: a client cannot tell that disabling an API's default endpoint took effect the way AWS defines it, nor that re-enabling restored it. Endpoint-lockdown and custom-domain-only cutover procedures key on 404/{"message":"Not Found"} to distinguish 'the default endpoint is off' from an authorization failure or a missing route; against the emulator that signal never arrives and the restore path is unobservable.

Fix: the execute-api front door's DisableExecuteApiEndpoint gate for HTTP APIs — the status and envelope it answers for a request to a disabled default endpoint, the fact that it refuses before invoking the integration, its per-API scoping (an enabled twin must be unaffected), and the path that returns the URL to serving when the member is cleared via UpdateApi.

Note on prior art: the open issue 20261114T024946Z-apigatewayv2-3-net-new-causes-a-disabled-execute.md describes this same cause as its Cause 1 and lists this check in its trace ids, but this replay did NOT mark the check as OWNED by it (its recorded replay key no longer matches the frame the check now times out in), so the check is re-filed here rather than dropped; a fixer may close the two together. Also related and NOT carrying this check's key: 20261110T153046Z-apigatewayv2-iam-lambda-logs-sts-updateapi-disableexecuteapiendpoint-true.md and 20261112T190000Z-apigatewayv2-the-disableexecuteapiendpoint-check-still-hangs-on-the-front-door-upgrade.md (both keyed to the apigatewayv2__iam__lambda__logs__sts WebSocket-twin check).

Traces: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.

## Evidence
Check apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore — verdict mismatch, detail 'Error: Check timed out after 60.0s'.

Expected (AWS): after UpdateApi(ApiId=<target>, DisableExecuteApiEndpoint=True) succeeds and GetApi reports DisableExecuteApiEndpoint=true on the target and false on the twin, every GET of the target's execute-api URL answers HTTP 404 with body {"message":"Not Found"} (fixed text, no variable part) and no backend marker anywhere in the body; the twin API's URL answers 200 with the backend's body in the same instant; no refused request's nonce ever appears in the backend's log (the integration was not invoked); then after UpdateApi(DisableExecuteApiEndpoint=False), GetApi reports false and the same target URL answers 200 with the backend's body again and the request does reach the backend.

Actual (emulator): no verdict inside the harness cap — the settled run of 404s the check polls for on the disabled target URL is never answered, and consequently the restore leg is never reached. The 404 status and the {"message":"Not Found"} envelope are the requirement; the per-run values in this check (API ids, the generated marker string, the per-request nonces) are environment values, not part of the contract.

## Steps to Reproduce
1. Boot the emulator. Create a Lambda function that echoes a marker from its environment.
2. Create two HTTP APIs (roles 'target' and 'twin'), each with an AWS_PROXY integration (PayloadFormatVersion 2.0) to that function, a route 'GET /hit' targeting the integration, and a $default stage with AutoDeploy=True; grant apigateway permission to invoke the function for both.
3. Confirm both APIs' execute-api URLs answer 200 with the backend marker on GET /hit.
4. Call UpdateApi(ApiId=<target>, DisableExecuteApiEndpoint=True); confirm GetApi reports true for the target and false for the twin.
5. GET the target's execute-api URL repeatedly: AWS answers HTTP 404 with body {"message":"Not Found"} and the backend is never invoked, while the twin's URL still answers 200 with the marker. Observe the emulator's status and body.
6. Call UpdateApi(ApiId=<target>, DisableExecuteApiEndpoint=False) and GET the same URL: expect a settled 200 with the marker and the request visible in the backend's log.
7. Re-replay apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.
