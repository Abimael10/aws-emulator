Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.apigw-lambda-response-streaming-proxy-integration-delivers-metadata-and-body apigateway__lambda.apigw-lambda-response-streaming-delivers-payload-past-the-10mb-rest-api-limit apigateway__lambda.apigw-lambda-response-streaming-malformed-output-rejected-with-502 apigateway__lambda.dotnet-rest-api-proxy-request-fields-and-response-shaping apigateway__lambda.go-handler-apigw-proxy-request-fields-and-response-shape-round-trip apigateway__lambda.java-apigw-proxy-request-fields-and-response-shaping-round-trip apigateway__lambda.ruby-apigw-rest-proxy-request-fields-and-response-shaping
Issue-Key: apigateway__lambda.apigw-lambda-response-streaming-proxy-integration-delivers-metadata-and-body|_poll_http#15|TimeoutError|raise TimeoutError(f"{url}: the run budget ended first; not graded")
Issue-Key: apigateway__lambda.apigw-lambda-response-streaming-delivers-payload-past-the-10mb-rest-api-limit|_poll_http#16|TimeoutError|raise TimeoutError(f"{url} never answered: the run budget ended first; not graded")
Issue-Key: apigateway__lambda.apigw-lambda-response-streaming-malformed-output-rejected-with-502|_poll_http#20|TimeoutError|raise TimeoutError(f"{url}: the run budget ended first; not graded")
Issue-Key: apigateway__lambda.dotnet-rest-api-proxy-request-fields-and-response-shaping|_poll#13|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: apigateway__lambda.go-handler-apigw-proxy-request-fields-and-response-shape-round-trip|_poll_http#15|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Key: apigateway__lambda.java-apigw-proxy-request-fields-and-response-shaping-round-trip|_poll_until#20|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: apigateway__lambda.ruby-apigw-rest-proxy-request-fields-and-response-shaping|_poll#16|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: 704534238375234ddf051963f5bbfba4730874a8

# apigateway__lambda: 5 net-new causes — a deployed REST stage never answers through an AWS_PROXY integration for a response-streaming function, nor for .NET, Go, Java or Ruby managed-runtime functions

> _Note: this issue was identified against the code base at commit `704534238375234ddf051963f5bbfba4730874a8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Five net-new causes, all on the execute-api data plane of a deployed REST stage whose method has an AWS_PROXY (Lambda proxy) integration. In every one the emulator accepts the whole control-plane build (CreateFunction, PutMethod/PutIntegration, CreateDeployment) and then serves nothing at the stage: the probe is polled until the run budget is exhausted and no HTTP response ever arrives, so no status, headers or body can be compared. The causes are separated by the integration feature each needs, because each can be fixed and closed independently.

Cause 1 of 5: a response-streaming Lambda behind an AWS_PROXY REST integration never answers.
AWS: a function wrapped in `awslambda.streamifyResponse` and fronted by an AWS_PROXY integration is invoked by API Gateway and its streamed result is relayed to the caller: the `awslambda.HttpResponseStream.from(stream, {statusCode, headers})` prelude becomes the HTTP status and response headers, the written chunks become the body, and a streamed body is documented to pass beyond the REST API's fixed, non-adjustable 10 MB "API Payload Size" quota (10,485,760 bytes). When the function emits output that is not a valid streaming prelude/body, API Gateway refuses the caller with HTTP 502 (recorded behavior only for the exact body of that 502). Expected: 200 with the prelude's content-type and the full body; 11 MiB delivered intact; 502 for malformed streamed output.
Emulator: the deployed stage never produces any response at all for such a function — all three probes (`/stream`, `/big`, and even the positive-control `/ok` of the malformed-output check) time out with no HTTP reply, so neither the 200 path, the >10 MB path nor the 502 refusal is observable.
Why: a client calling a streaming-backed REST endpoint hangs instead of receiving a status; nothing distinguishes "still streaming" from "never going to answer", and a caller relying on the 502 for malformed streamed output sees a hung socket instead of a refusal it can retry or report.
Fix: the execute-api data-plane invocation path for AWS_PROXY integrations where the Lambda returns a streaming response — the relay that must consume the function's stream, parse the HttpResponseStream prelude into status+headers, forward chunks without applying the 10 MB REST payload cap, and map an unparseable prelude to 502.
Traces: apigateway__lambda.apigw-lambda-response-streaming-proxy-integration-delivers-metadata-and-body, apigateway__lambda.apigw-lambda-response-streaming-delivers-payload-past-the-10mb-rest-api-limit, apigateway__lambda.apigw-lambda-response-streaming-malformed-output-rejected-with-502.

Cause 2 of 5: a .NET managed-runtime function behind AWS_PROXY never answers at the stage.
AWS: a .NET Lambda handler fronted by AWS_PROXY receives the API Gateway proxy request event (httpMethod, path, pathParameters, queryStringParameters, headers, body, isBase64Encoded) and the object it returns (statusCode, headers, body, isBase64Encoded) is shaped into the HTTP response the caller gets. Expected: the probe GET answers 200 with the handler's shaped headers and body.
Emulator: GET /probe through the AWS_PROXY integration never answers; the poll consumes the budget with no response.
Why: every .NET-based REST API served by the emulator is unreachable — callers hang rather than receive the handler's response, and request-field/response-shaping contracts cannot be relied on at all.
Fix: the Lambda execution path for the .NET managed runtime as reached from the execute-api AWS_PROXY relay (whichever of the two never completes the invoke).
Traces: apigateway__lambda.dotnet-rest-api-proxy-request-fields-and-response-shaping.

Cause 3 of 5: a Go handler behind AWS_PROXY never answers at the stage.
AWS: the Go Lambda handler receives the same proxy request event and its returned response struct becomes the HTTP response; path parameters (`/items/{id}` -> 42), request headers and the response's headers/body round-trip. Expected: REST API GET /items/42 answers with the handler's status, headers and body.
Emulator: the GET never answers; the poll exhausts the run budget with no HTTP reply.
Why: Go-backed proxy APIs are unreachable; a client gets a hang instead of a response.
Fix: the Lambda execution path for the Go runtime as reached from the execute-api AWS_PROXY relay.
Traces: apigateway__lambda.go-handler-apigw-proxy-request-fields-and-response-shape-round-trip.

Cause 4 of 5: a Java handler behind AWS_PROXY never answers at the stage.
AWS: a `RequestHandler<Map<String,Object>, Map<String,Object>>` receives the AWS_PROXY request as a generic Map and the Map it returns is honoured for statusCode, headers and body. Expected: GET /_aws/execute-api/<api>/live/echo?x=... answers with the handler's shaped response.
Emulator: the GET on the deployed `live` stage never answers; the poll exhausts the run budget.
Why: Java-backed proxy APIs are unreachable; callers hang.
Fix: the Lambda execution path for the Java runtime as reached from the execute-api AWS_PROXY relay.
Traces: apigateway__lambda.java-apigw-proxy-request-fields-and-response-shaping-round-trip.

Cause 5 of 5: a Ruby handler behind AWS_PROXY never answers at the stage.
AWS: the Ruby handler receives the AWS_PROXY event hash and the hash it returns is shaped into the HTTP response, so the deployed stage starts answering 200 once the deployment exists. Expected: the deployed stage answers 200.
Emulator: the deployed stage never starts answering 200 — every poll times out with no response.
Why: Ruby-backed proxy APIs are unreachable; callers hang.
Fix: the Lambda execution path for the Ruby runtime as reached from the execute-api AWS_PROXY relay.
Traces: apigateway__lambda.ruby-apigw-rest-proxy-request-fields-and-response-shaping.

Related but not covering: the open issues 20261023T170254Z-apigateway-lambda-three-integration-behaviour-checks-never-complete.md, 20261023T210711Z-apigateway-lambda-four-integration-authorizer-behaviour-checks-hang.md, 20261103T031416Z-apigateway-lambda-http-proxy-integrations-pointed-at-a.md and 20261107T191316Z-apigateway-lambda-3-net-new-causes-non-proxy-d346f3.md describe other hanging/mis-answering integration scenarios (stage variables, authorizers, HTTP_PROXY to a function URL, non-proxy AWS integrations) and none of them carries the replay keys listed here. 20261027T180000Z-lambda-invokewithresponsestream-refuses-every-real-streaming-invocation.md is about the lambda InvokeWithResponseStream API, not the apigateway AWS_PROXY relay. The suite marked none of the seven checks below as already filed or owned.

## Evidence
All seven failing checks end in the same way: the control plane succeeds, then the execute-api probe is polled until the budget ends with no HTTP response ever returned.

Cause 1 (response streaming):
- apigw-lambda-response-streaming-proxy-integration-delivers-metadata-and-body: `TimeoutError: https://<host>/_aws/execute-api/<api-id>/test/stream: the run budget ended first; not graded` (expected 200 + prelude headers + body).
- apigw-lambda-response-streaming-delivers-payload-past-the-10mb-rest-api-limit: `TimeoutError: https://<host>/_aws/execute-api/<api-id>/test/big never answered: the run budget ended first; not graded` (expected 200 and 11 MiB = 11,534,336 bytes of body, past the fixed 10,485,760-byte REST payload quota).
- apigw-lambda-response-streaming-malformed-output-rejected-with-502: `TimeoutError: https://<host>/_aws/execute-api/<api-id>/test/ok: the run budget ended first; not graded` — the positive control never answers, so the expected HTTP 502 for malformed streamed output is never reached either.

Cause 2 (.NET): `TimeoutError: GET /probe answers through the AWS_PROXY integration: the run budget ended first; not graded`.
Cause 3 (Go): `TimeoutError: REST API GET /items/42: the run budget ended first; not graded`.
Cause 4 (Java): `TimeoutError: GET https://<host>/_aws/execute-api/<api-id>/live/echo?x=hello123: the run budget ended first; not graded`.
Cause 5 (Ruby): `TimeoutError: the deployed stage starts answering 200: the run budget ended first; not graded`.

Expected in every case: an HTTP response from the deployed stage carrying the status, headers and body the AWS_PROXY handler returned. Actual: no response at all — not a wrong status, not a 5xx; the connection never yields a status line. Hosts, ports and generated API ids above vary per run and are not part of the requirement.

## Steps to Reproduce
Common setup (per cause): boot the emulator; CreateFunction with the runtime named by the cause; CreateRestApi, a resource and method with an AWS_PROXY integration pointing at that function, add the lambda:InvokeFunction permission, CreateDeployment onto a stage.

1. Cause 1: deploy a Node function whose handler is wrapped in awslambda.streamifyResponse and emits an awslambda.HttpResponseStream prelude {statusCode:200, headers:{content-type:text/plain}} followed by written chunks. GET the deployed path. Observe the request never receives any HTTP response. Repeat with 11 x 1 MiB chunks (expect 200 and the full 11 MiB body, past the 10 MB REST payload quota) and with a handler emitting a malformed prelude (expect HTTP 502).
2. Cause 2: deploy a .NET handler that echoes the proxy request fields and returns {statusCode, headers, body}; GET /probe on the deployed stage. No response arrives.
3. Cause 3: deploy a Go handler; GET /items/42 on the deployed stage. No response arrives.
4. Cause 4: deploy a Java RequestHandler<Map,Map>; GET /_aws/execute-api/<api>/live/echo?x=hello123. No response arrives.
5. Cause 5: deploy a Ruby handler; poll the deployed stage for a 200. It never answers.
