Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests
Issue-Key: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests|run#21|PublishedUrlRejected|url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/"
Issue-Commit: c7bc1d610827f184f11257fe9f128e3fbb77240c

# REOPENS: an HTTP API's published ApiEndpoint is still an <apiId>.execute-api.<region>.amazonaws.com host the emulator does not serve, so a stage's DefaultRouteSettings throttle can never be exercised

> _Note: this issue was identified against the code base at commit `c7bc1d610827f184f11257fe9f128e3fbb77240c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
REOPENS 20261109T152222Z-apigateway-apigatewayv2-an-http-api-s-apiendpoint-is.md: its fix left the call refused.

Cause 1 of 1: the value apigatewayv2 publishes as the HTTP API's invoke address is an AWS production host, not an address the emulator is reached on, so the published endpoint cannot be called at all.

AWS: Smithy apigatewayv2 CreateApiResponse.ApiEndpoint is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." — i.e. ApiEndpoint is the address a client invokes, and appending the stage/route path to it reaches the API's data plane. For an emulator the requirement is on reachability, not on the literal AWS name: recorded behavior only — a value the emulator publishes as an invoke address has to be a host the emulator itself serves (http(s)://<own host>:<port>/<path>), because a client follows it verbatim. With DefaultRouteSettings ThrottlingRateLimit=1.0 / ThrottlingBurstLimit=1 on the $default stage, that call is answered either 200 by the integration or 429 with the default THROTTLED body {"message":"Too Many Requests"} (fixed text; no variable parts).

Emulator: CreateApi returns ApiEndpoint as "https://<apiId>.execute-api.<region>.amazonaws.com" (one run showed https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com — the api-id, region and port are per-run values, the HOST FORM is the defect). The emulator announced an address other than the one it is reached on, so the request is refused at egress before any HTTP request is made: "endpoint_url was given https://<apiId>.execute-api.<region>.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". No status, error code or body is produced at all because nothing is sent.

Why: the documented way to invoke an HTTP API — read ApiEndpoint, append the stage/route path, issue the request — cannot work. Everything behind the endpoint is unreachable, so the stage-level throttle behaviour (partial shedding with the default 429 "Too Many Requests" envelope and recovery once the bucket refills) can never be observed. Note the separate open issue 20261110T163347Z-apigateway-apigatewayv2-an-http-api-default-stage-throttle.md describes the throttle DECISION (every request of a burst shed); it does not cover this cause and cannot be confirmed while the endpoint is unreachable, because the check now fails before it reaches the data plane.

Fix: the apigatewayv2 CreateApi/GetApi/GetApis serializers (the ApiEndpoint member) must publish a host the emulator serves, and the execute-api HTTP API data plane must answer on that host for the API's stages and routes.

Traces: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests.

## Evidence
Replay (apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests):
```
File "context.py", line 617, in _published_target
  raise PublishedUrlRejected(...)
simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:46433/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
```
Expected: CreateApi's ApiEndpoint names an emulator-served host, so `GET <ApiEndpoint>/$default/` (stage path appended) reaches the HTTP API data plane and is answered 200 with the HTTP_PROXY backend body, or 429 with exactly {"message":"Too Many Requests"} once the rate=1/burst=1 stage throttle sheds.
Actual: ApiEndpoint is an *.execute-api.<region>.amazonaws.com host, so the request never leaves the client and no response is produced. (Host id mp67t4dbd4, region us-west-2 and port 46433 are per-run values, not part of the requirement.)

This exact refusal is what 20261109T152222Z-apigateway-apigatewayv2-an-http-api-s-apiendpoint-is.md (status: fixed, Issue-Key ...|PublishedUrlRejected|url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/") claimed to fix; the same check is refused the same way again.

## Steps to Reproduce
1. apigateway: create a REGIONAL REST API with a MOCK GET on / returning {"backend":"served"} and deploy it to a stage — the HTTP_PROXY backend.
2. apigatewayv2 CreateApi(Name=..., ProtocolType="HTTP"); CreateIntegration(IntegrationType="HTTP_PROXY", IntegrationMethod="GET", IntegrationUri=<backend stage URL>, PayloadFormatVersion="1.0"); CreateRoute(RouteKey="GET /", Target=integrations/<id>).
3. CreateStage(StageName="$default", AutoDeploy=true, DefaultRouteSettings={ThrottlingRateLimit: 1.0, ThrottlingBurstLimit: 1}).
4. Read ApiEndpoint from the CreateApi response and GET <ApiEndpoint>/.
Expected: the request reaches the emulator's HTTP API data plane (200 from the backend, or 429 {"message":"Too Many Requests"} under burst).
Actual: ApiEndpoint is https://<apiId>.execute-api.<region>.amazonaws.com, a host the emulator does not serve, so the call is refused at egress and never made.
