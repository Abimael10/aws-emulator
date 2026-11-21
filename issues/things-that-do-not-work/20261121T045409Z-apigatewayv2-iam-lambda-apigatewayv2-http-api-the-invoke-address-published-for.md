Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__iam__lambda.http-api-head-and-undeclared-method-dispatch-and-forwarded-headers apigatewayv2__iam__lambda.http-api-request-target-decoded-twice-and-normalised-before-route-selection
Issue-Key: apigatewayv2__iam__lambda.http-api-head-and-undeclared-method-dispatch-and-forwarded-headers|_leg_request_context#19|AssertionError|assert first_label == api_id, (
Issue-Key: apigatewayv2__iam__lambda.http-api-request-target-decoded-twice-and-normalised-before-route-selection|_poll#19|Timeout|time.sleep(3)
Issue-Commit: 2f215cd8a92428a081069d88561cad2cfdd06f78

# apigatewayv2 HTTP API: the invoke address published for an API is a loopback host whose first label is not the API id, and the two-pass request-target decode still never settles (2 causes)

> _Note: this issue was identified against the code base at commit `2f215cd8a92428a081069d88561cad2cfdd06f78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
2 net-new root causes in the apigatewayv2 HTTP API surface, both grounded in failing checks the suite marked neither ALREADY FILED nor OWNED. Related issues exist but none carries these replay keys: open 20261118T125928Z-apigatewayv2-cloudcontrol-cloudformation-apigatewayv2-publishes-getapi-fn-getatt-apiendpoint-as-a.md reports the same ApiEndpoint publication defect from the CloudFormation/Cloud Control side only, and 20261109T224859Z-apigatewayv2-iam-lambda-apigatewayv2-http-api-execute-api-data-plane-2.md is marked FIXED (a resolved issue never covers a failure) — its cause 2 was the decode pass that cause 2 below shows is still unobservable.

Cause 1 of 2: the address an HTTP API is invoked on is a loopback host, so it is not the API's own hostname and its first label is not the API id. Failing check id: apigatewayv2__iam__lambda.http-api-head-and-undeclared-method-dispatch-and-forwarded-headers.
AWS: CreateApi (POST /v2/apis, 201) and GetApi return the member ApiEndpoint (Smithy: CreateApiResponse.ApiEndpoint, target __string, jsonName apiEndpoint) alongside ApiId (Smithy: Id). For an HTTP API with the $default stage AWS publishes it as https://{api-id}.execute-api.{region}.amazonaws.com — a DNS name whose FIRST LABEL is exactly the value of ApiId — and the data plane serves that name. The same identity is echoed into the proxy payload: requestContext.domainName is that host and requestContext.domainPrefix is that first label, i.e. the API id. Requirement parts: the fixed structure `{ApiId}.execute-api.{region}.amazonaws.com` (and the https scheme), with {ApiId} taken from the response and {region} from the request's endpoint — the id value itself is minted per run and is not part of the requirement.
Emulator: the address the API is reachable on has host 127.0.0.1 (a loopback literal, path-style, with no api-id label at all), so the check stops with `AssertionError: the invoke host '127.0.0.1' must be this API's own hostname, whose first label is the API id 'mp67t4dbd4'` ('mp67t4dbd4' is this run's generated id and is not part of the requirement). Because the address is not the API's own hostname, the requestContext leg it gates — domainName == the invoke host, domainPrefix == the API id, plus apiId/stage/routeKey/method/path/protocol/userAgent on a served GET /ctx — is never graded either.
Why: a client that derives the invoke address from ApiEndpoint (or from Fn::GetAtt ApiEndpoint) gets an address with no API identity in it, so SigV4 credential scope, the Host/SNI value, per-API DNS and custom-domain mapping all key off the wrong name; a handler that recovers the API id from requestContext.domainPrefix or logs requestContext.domainName sees a loopback literal instead of the API's host.
Fix: the apigatewayv2 control-plane serializer that computes ApiEndpoint for CreateApi/GetApi (and the Fn::GetAtt path that mirrors it), together with the execute-api front door's host-based routing so `{api-id}.execute-api.{region}.amazonaws.com` is both announced and served, and the data plane's requestContext builder so domainName/domainPrefix are derived from that host.
Traces: apigatewayv2__iam__lambda.http-api-head-and-undeclared-method-dispatch-and-forwarded-headers.

Cause 2 of 2: the HTTP API request target is still not decoded twice and normalised before route selection, and the check that measures it reaches no verdict at all — it burns its whole budget. Failing check id: apigatewayv2__iam__lambda.http-api-request-target-decoded-twice-and-normalised-before-route-selection.
AWS: recorded behavior only — the execute-api data plane is not in the apigatewayv2 Smithy model (the model binds control-plane operations such as CreateApi/CreateRoute/CreateStage). The recording shows an HTTP API decoding the request target exactly twice before route selection: /items/a%41b selects `GET /items/{id}` with rawPath /items/aAb and pathParameters {"id":"aAb"}; /items/a%2541b reaches the same decoded form on the second pass; /items/a%252541b stops at rawPath /items/a%41b. The second pass refuses: a target that decodes to a malformed escape (/items/a%25b) is answered 400 with the gateway's own envelope body {"message":"Bad Request"} (fixed text, no variable parts), while /items/a%2525b is served 200. %2F does not separate path segments, a trailing slash is significant, and dot segments are normalised before matching.
Emulator: the replay reports only `Error: Check timed out after 60.0s` — the check reaches no verdict. Hypothesis: its legs poll the stage for the decoded rawPath/pathParameters and for the 400 Bad Request envelope, and because neither the decode passes nor that refusal ever materialise, each leg retries until the budget ends rather than failing on a value; the previously filed (and closed) finding on this exact check reported rawPath coming back as the untouched request target, which is consistent with nothing on this path having changed.
Why: a handler that reads rawPath/pathParameters receives percent-encoded bytes AWS had already decoded, so path parameters captured from encoded segments are wrong, route selection diverges for encoded targets, and a request AWS refuses 400 for a malformed escape is served instead. Because the scenario never terminates, a client also gets no verdict at all on the whole decode/normalise contract.
Fix: the execute-api HTTP-API route-selection path — decode the request target in two passes (refusing a malformed escape on either pass with the gateway's own 400 {"message":"Bad Request"} envelope) before matching route keys, and build rawPath, requestContext.http.path and pathParameters from the decoded target; %2F must not split segments, the trailing slash must stay significant and dot segments must be normalised.
Traces: apigatewayv2__iam__lambda.http-api-request-target-decoded-twice-and-normalised-before-route-selection.

## Evidence
Cause 1 — check apigatewayv2__iam__lambda.http-api-head-and-undeclared-method-dispatch-and-forwarded-headers (verdict: mismatch):
Expected (recorded AWS): the API is invoked on its own hostname, https://{ApiId}.execute-api.{region}.amazonaws.com, so splitting that address yields a host whose first label equals the ApiId returned by CreateApi; the served GET /ctx payload then reports requestContext.domainName == that host, requestContext.domainPrefix == that first label, requestContext.apiId == ApiId, stage == "$default", routeKey == "GET /ctx", http.method == "GET", http.path == "/ctx", protocol == "HTTP/1.1".
Actual (emulator): the invoke address host is the loopback literal 127.0.0.1 (path-style), and the check aborts at its last leg with:
  AssertionError: the invoke host '127.0.0.1' must be this API's own hostname, whose first label is the API id 'mp67t4dbd4'
Earlier legs of the same check (warm-up GET /items answered 200 twice, HEAD/undeclared-method 404s, forwarded headers, absent-vs-empty payload keys) had already passed in this run, so the divergence is isolated to the published/served invoke address. Fixed text vs variable parts: the structural rule `host == {ApiId}.execute-api.{region}.amazonaws.com` is the requirement; the id 'mp67t4dbd4', the region and any port are run values.

Cause 2 — check apigatewayv2__iam__lambda.http-api-request-target-decoded-twice-and-normalised-before-route-selection (verdict: mismatch):
Expected (recorded AWS): GET /items/a%41b -> 200, routeKey "GET /items/{id}", rawPath "/items/aAb", pathParameters {"id":"aAb"}; GET /items/a%2541b -> same decoded form; GET /items/a%252541b -> rawPath "/items/a%41b"; GET /items/a%25b -> 400 with body {"message":"Bad Request"}; GET /items/a%2525b -> 200.
Actual (emulator): no response value is reported at all — the suite's whole detail for this check is:
  Error: Check timed out after 60.0s
so the stage never produced the decoded rawPath nor the 400 Bad Request envelope the legs wait for inside the run budget.

## Steps to Reproduce
Cause 1:
1. CreateApi(Name=<n>, ProtocolType="HTTP") and note ApiId and ApiEndpoint; create routes `GET /items`, `GET /ctx`, `ANY /any/{proxy+}` (plus a second backend on `GET /hd`, `HEAD /hd`, `ANY /anyhd`) with Lambda AWS_PROXY integrations, and the `$default` stage with AutoDeploy=true.
2. Split the API's invoke address (ApiEndpoint for the $default stage): observe the host is 127.0.0.1 instead of {ApiId}.execute-api.{region}.amazonaws.com, so its first label is '127' and not the ApiId.
3. Send GET /ctx to the stage and read the proxy payload: requestContext.domainName / domainPrefix cannot name the API because the API has no own hostname to serve.
4. Equivalent: re-replay check apigatewayv2__iam__lambda.http-api-head-and-undeclared-method-dispatch-and-forwarded-headers.

Cause 2:
1. CreateApi (HTTP) with routes exactly `GET /items`, `GET /items/{id}`, `GET /pets/{proxy+}`, `GET /a/b`, `GET /b`, `GET /q` — no `$default`, no root greedy route — behind a Lambda proxy (payload 2.0) that echoes routeKey, rawPath, pathParameters, rawQueryString; create the `$default` stage with AutoDeploy=true.
2. GET /items/a%41b -> expect rawPath /items/aAb and pathParameters {"id":"aAb"}.
3. GET /items/a%2541b -> expect the same decoded form; GET /items/a%252541b -> expect rawPath /items/a%41b.
4. GET /items/a%25b -> expect 400 {"message":"Bad Request"}; GET /items/a%2525b -> expect 200.
5. Observe that the legs never obtain these answers and the check is killed at its budget.
6. Equivalent: re-replay check apigatewayv2__iam__lambda.http-api-request-target-decoded-twice-and-normalised-before-route-selection.
