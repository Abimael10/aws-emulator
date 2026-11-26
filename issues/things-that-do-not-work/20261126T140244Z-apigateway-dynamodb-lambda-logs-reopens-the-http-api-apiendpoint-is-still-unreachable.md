Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-missing-permission-negative
Issue-Key: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive|run#14|PublishedUrlRejected|url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/echo?msg=hello123"
Issue-Key: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-missing-permission-negative|_poll_http_get_any_terminal#24|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# REOPENS: the HTTP API ApiEndpoint is still unreachable — it is now published as <apiId>.execute-api.<region>.localhost:<port>, so neither the Lambda proxy integration nor the missing-permission path is ever invoked

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
REOPENS 20261109T143515Z-apigateway-dynamodb-lambda-logs-http-api-apiendpoint-is-published-as-id-execute.md: its fix left the call refused. That resolved issue's cause 1 carries exactly these two checks' replay keys for this same cause (the published ApiEndpoint is not an address the emulator serves); the host form changed from `*.execute-api.<region>.amazonaws.com` to `*.execute-api.<region>.localhost:<port>`, but both checks are still refused at egress and never reach the data plane, so the behaviour is re-filed with their keys.

Cause 1 of 1: apigatewayv2 publishes the HTTP API's invoke address as a synthesized name in the `.localhost` zone that no client can resolve to this emulator, so a client that follows the published endpoint cannot invoke the API's routes. Failing check ids: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive, apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-missing-permission-negative.

AWS: Smithy apigatewayv2 Api$ApiEndpoint (returned by CreateApi and GetApi, target __string) is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." — it is the address a client dials, and AWS serves that name. For an emulator the requirement is reachability rather than the literal AWS name: recorded behavior only — a value published as an invoke address has to be an address the emulator itself answers on (http(s)://<own host>:<port>/<path>), because a client follows it verbatim. Behind a reachable endpoint the recordings show a $default-stage GET on an AWS_PROXY route being answered by the Lambda (200 with the function's body, request echoed through the proxy payload) and, when the function carries no lambda:InvokeFunction grant for apigateway, the stage answering 500 with the gateway's own envelope body {"message":"Internal Server Error"} and the function never being invoked.

Emulator: CreateApi answers ApiEndpoint as `https://{api-id}.execute-api.{region}.localhost:{port}` — the api-id, region and port are per-run values; the HOST FORM (a synthesized three-label name in the RFC 6761 `.localhost` zone) is the defect, since nothing resolves that name to this process and it is not one of the emulator's own hosts. Both checks are refused at egress before any HTTP request is made: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<apiId>.execute-api.<region>.localhost:<port>[/hit] (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. No status, error code or body is produced, because nothing is sent.

Why: the invoke URL is the one value a client must be able to trust. Every control-plane call (CreateApi, CreateIntegration, CreateRoute, CreateStage, AddPermission) succeeds and the API still cannot be called, so neither the positive AWS_PROXY delivery (payload fields, echoed query string, the function's 200 body) nor the missing-permission refusal (502/500 envelope with the function never invoked) can be observed or relied on.

Fix: the apigatewayv2 ApiEndpoint publication — the authority builder behind CreateApi/GetApi/GetApis must announce the address the invoke-plane listener actually landed on (its own host and port, path-style if that is how the front door serves the plane) rather than folding the api id, the region and an `execute-api` label into a `.localhost` name, and the execute-api front door must answer that published address for the api's stages and routes.

Traces: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive, apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-missing-permission-negative. Code sites: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/create_api.rs lines 486-500 and services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state/invoke_plane_transport.rs lines 102-139 (`announced_endpoint` / `announced_authority`).

## Evidence
Replay (apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive, verdict: mismatch):
```
File "context.py", line 616, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:37073 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:42285/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
```
The target is built from the emulator's own response: `url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/echo?msg=hello123"`.

Replay (apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-missing-permission-negative, verdict: mismatch):
```
simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:44851/hit (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:41149/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
```
The check reads `endpoint = api["ApiEndpoint"]` from CreateApi and routes it through ctx.endpoint_route().

Expected: ApiEndpoint names an emulator-served address, so GET <ApiEndpoint>/echo?msg=hello123 is answered 200 by the AWS_PROXY Lambda with the echoed message, and the un-granted twin is answered by the gateway's own 5xx envelope with the function never invoked.
Actual: both calls are refused at egress; no response is produced at all. (The id `mp67t4dbd4`, the region and the ports are per-run values; the host FORM is the requirement.)

## Steps to Reproduce
1. lambda: CreateFunction (python runtime) with a handler that echoes the proxy payload / returns {"statusCode":200,...}.
2. apigatewayv2 CreateApi(Name=<n>, ProtocolType="HTTP"); CreateIntegration(IntegrationType="AWS_PROXY", PayloadFormatVersion="2.0", IntegrationUri=<function arn>); CreateRoute(RouteKey="GET /echo" resp. "GET /hit", Target="integrations/<id>"); CreateStage(StageName="$default", AutoDeploy=true). For the positive leg add the lambda:InvokeFunction permission for apigateway.amazonaws.com; for the negative leg omit it.
3. Read ApiEndpoint from the CreateApi response: observe `https://<apiId>.execute-api.<region>.localhost:<port>`.
4. GET <ApiEndpoint>/echo?msg=hello123 (and <ApiEndpoint>/hit for the negative leg). Expected: 200 with the echoed payload, resp. the gateway's own 5xx envelope with no invocation. Actual: the address is not one the emulator is reached on, so the call is refused before it is made.
5. Equivalent: re-replay checks apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive and apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-missing-permission-negative.
