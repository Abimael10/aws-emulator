Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.apigw-mock-integration-serves-cors-preflight
Issue-Key: apigateway__lambda.apigw-mock-integration-serves-cors-preflight|_poll_http#24|Timeout|time.sleep(interval_s)
Issue-Commit: c31be09f961a7f044ea59076daac7fb377aa83e8

# apigateway: a MOCK integration never answers a CORS preflight OPTIONS — the execute-api request never returns 200, so the check hangs to its 60 s budget

> _Note: this issue was identified against the code base at commit `c31be09f961a7f044ea59076daac7fb377aa83e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutIntegrationResponse

## Description
One net-new root cause.

Cause 1 of 1: a deployed REST API resource with a MOCK integration on OPTIONS never answers the preflight request on the execute-api data plane.

AWS: recorded behavior only for the data-plane leg; the control-plane shapes are modelled. A resource configured with PutMethod(httpMethod=OPTIONS, authorizationType=NONE), PutIntegration(type=MOCK, requestTemplates={"application/json": "{\"statusCode\": 200}"}), PutMethodResponse(statusCode=200, responseParameters naming method.response.header.Access-Control-Allow-Origin / -Allow-Methods / -Allow-Headers / -Max-Age) and PutIntegrationResponse(statusCode=200, responseParameters mapping each of those headers to a single-quoted VTL literal) is answered by API Gateway itself, with NO backend: once the stage is deployed, an OPTIONS request to https://<api-id>.execute-api.<region>.amazonaws.com/<stage>/items carrying Origin, Access-Control-Request-Method and Access-Control-Request-Headers returns HTTP 200 with each declared Access-Control-* response header present, its value being the literal from the integration response with the surrounding single quotes STRIPPED (so Access-Control-Allow-Origin is the bare origin, Access-Control-Max-Age is the bare number). This is AWS's documented 'Enabling CORS for a REST API resource' flow.

Emulator: the OPTIONS request is polled for a 200 and never gets one. The check reaches no response at all inside the harness budget and is reported as `Error: Check timed out after 60.0s`. Nothing downstream of that poll (the header-rendering assertions, the GET through the AWS_PROXY integration on the same resource) is ever evaluated, so the MOCK integration is not merely rendering headers wrongly — the request does not complete.

Why: a client relying on AWS's behavior sends a browser preflight to a CORS-enabled REST resource and expects an immediate 200 with the Access-Control-* headers. Against the emulator the preflight hangs, so every browser-origin call to the API fails before the real request is ever sent; a MOCK integration — the normal way to implement an OPTIONS preflight and also the normal way to serve a static canned response with no backend — is unusable.

Fix: the execute-api data-plane request router and its MOCK integration path, plus the integration-response renderer that turns a method/integration response's single-quoted VTL literals into HTTP response headers. The OPTIONS method must be dispatched to the MOCK integration (not to a backend lookup that never resolves), answered with the integration response's statusCode, and each method.response.header.<Name> parameter emitted with the literal unquoted.

Traces: apigateway__lambda.apigw-mock-integration-serves-cors-preflight. Related but NOT the same cause and NOT covering this check: 20261107T184737Z-apigateway-cloudformation-lambda-the-execute-api-data-plane-refuses-every-call.md (that one is about the data plane refusing calls on a CloudFormation-deployed API).

## Evidence
Expected (AWS): OPTIONS https://<api-id>.execute-api.<region>.amazonaws.com/test/items with Origin: <origin>, Access-Control-Request-Method: GET, Access-Control-Request-Headers: content-type -> HTTP 200, headers access-control-allow-origin: <origin> (no quotes), access-control-allow-methods containing GET, access-control-allow-headers containing Content-Type, access-control-max-age: 600.

Actual (emulator): the poll for a 200 from that OPTIONS request never succeeds; the suite reports the check as `Error: Check timed out after 60.0s` with no HTTP status, no headers and no body recorded — the preflight never completes.

## Steps to Reproduce
1. CreateRestApi, CreateResource(pathPart=items).
2. PutMethod(OPTIONS, authorizationType=NONE); PutIntegration(OPTIONS, type=MOCK, requestTemplates={"application/json": "{\"statusCode\": 200}"}).
3. PutMethodResponse(OPTIONS, 200, responseParameters = {method.response.header.Access-Control-Allow-Origin: false, ...-Allow-Methods: false, ...-Allow-Headers: false, ...-Max-Age: false}).
4. PutIntegrationResponse(OPTIONS, 200, responseParameters = {method.response.header.Access-Control-Allow-Origin: "'https://example'", ...-Allow-Methods: "'GET,OPTIONS'", ...-Allow-Headers: "'Content-Type,Authorization'", ...-Max-Age: "'600'"}, responseTemplates={"application/json": ""}).
5. CreateDeployment to stage `test`.
6. Send OPTIONS to https://<api-id>.execute-api.<region>.amazonaws.com/test/items with the three preflight request headers.
Observed: no response — the request never yields 200 (check times out after 60 s). Expected: 200 with the four Access-Control-* headers, values unquoted.
