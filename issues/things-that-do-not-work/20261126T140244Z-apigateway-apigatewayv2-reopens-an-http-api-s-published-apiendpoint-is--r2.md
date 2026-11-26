Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests
Issue-Key: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests|run#21|PublishedUrlRejected|url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/"
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# REOPENS: an HTTP API's published ApiEndpoint is now a synthesized <apiId>.execute-api.<region>.localhost:<port> host that is still not an address the emulator is reached on, so a stage's DefaultRouteSettings throttle can never be exercised

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
REOPENS 20261114T171242Z-apigateway-apigatewayv2-reopens-an-http-api-s-published-apiendpoint-is.md: its fix left the call refused. That resolved issue carries this check's replay key for exactly this cause (the published ApiEndpoint is not an address the emulator serves); the host form changed from `*.execute-api.<region>.amazonaws.com` to `*.execute-api.<region>.localhost:<port>`, but the published value is still refused at egress and the check still never reaches the data plane, so the behaviour is re-filed with its key.

Cause 1 of 1: apigatewayv2 publishes the HTTP API's invoke address as a synthesized name in the `.localhost` zone that no client can resolve to this emulator, so the documented way of invoking an HTTP API (read ApiEndpoint, append the stage/route path, issue the request) cannot be used at all. Failing check id: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests.

AWS: Smithy apigatewayv2 Api$ApiEndpoint (also CreateApiResponse$ApiEndpoint / GetApiResponse$ApiEndpoint, target __string) is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." — ApiEndpoint is the address a client dials, and AWS serves that name. For an emulator the requirement is on REACHABILITY, not on the literal AWS name: recorded behavior only — a value the emulator publishes as an invoke address must be an address the emulator itself answers on and a client can dial verbatim (http(s)://<own host>:<port>/<path>, e.g. the loopback literal and port of its own listener). With the $default stage carrying DefaultRouteSettings ThrottlingRateLimit=1.0 / ThrottlingBurstLimit=1, a GET on that address is then answered either 200 by the HTTP_PROXY integration or 429 with the default THROTTLED envelope body {"message":"Too Many Requests"} (fixed text, no variable parts), and serves again once the bucket refills.

Emulator: CreateApi answers ApiEndpoint as `https://{api-id}.execute-api.{region}.localhost:{port}` — the api-id, region and port are per-run values; the HOST FORM (a synthesized three-label name in the RFC 6761 `.localhost` zone) is the defect, because nothing resolves that name to this process and the emulator's own hosts do not include it. The call is therefore refused at egress before any HTTP request is made: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<apiId>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. No status, error code or body is produced at all, because nothing is sent.

Why: a client that honours the one value it must trust — the published invoke URL — cannot reach any deployed HTTP API, even though every control-plane call (CreateApi, CreateIntegration, CreateRoute, CreateStage) succeeded. Everything behind the endpoint is unobservable, so the stage-level throttle contract (partial shedding of a concurrent burst with the exact default 429 {"message":"Too Many Requests"} envelope, non-shed requests still reaching the backend, and recovery once the bucket refills) can never be exercised.

Fix: the apigatewayv2 ApiEndpoint publication — the authority builder behind CreateApi/GetApi/GetApis must announce the address the invoke-plane listener actually landed on (its own host and port, path-style if that is how the front door serves the plane) instead of folding the api id, the region and an `execute-api` label into a `.localhost` name, and the execute-api front door must answer that published address for the api's stages and routes.

Traces: apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests. Code sites: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/create_api.rs lines 486-500 (chooses the announced endpoint) and services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state/invoke_plane_transport.rs lines 102-139 (`announced_endpoint` / `announced_authority`, which build `{api-id}.execute-api.{region}.<announced zone>:<port>`; its own unit test at lines 150-217 pins the `.localhost` form).

## Evidence
Replay (apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests, verdict: mismatch):
```
File "context.py", line 616, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
File "context.py", line 632, in _published_target
    raise PublishedUrlRejected(f"{message}: {problem[1]}", problem[0], value_class(value))
simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:42109 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:41517/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
```
The check builds the target as `url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/"` from the CreateApi response, so the rejected value is the emulator's own published ApiEndpoint.

Expected: ApiEndpoint names an address the emulator serves, so `GET <ApiEndpoint>/` on the $default stage is answered 200 with the HTTP_PROXY backend body, and under a rate=1/burst=1 DefaultRouteSettings burst at least one request is answered 429 with exactly {"message":"Too Many Requests"} while the rest still reach the backend.
Actual: the published host is `<apiId>.execute-api.<region>.localhost:<port>`, which is not one of the emulator's own hosts, so the request never leaves the client and no response at all is produced. (The id `mp67t4dbd4`, the region `us-west-2` and the ports 42109/41517 are per-run values and are not part of the requirement; the host FORM is.)

## Steps to Reproduce
1. apigateway: create a REGIONAL REST API with a MOCK GET on / returning {"backend":"served"} and deploy it to a stage — this is the HTTP_PROXY backend.
2. apigatewayv2 CreateApi(Name=<n>, ProtocolType="HTTP"); CreateIntegration(IntegrationType="HTTP_PROXY", IntegrationMethod="GET", IntegrationUri=<backend stage URL>, PayloadFormatVersion="1.0"); CreateRoute(RouteKey="GET /", Target="integrations/<id>").
3. CreateStage(StageName="$default", AutoDeploy=true, DefaultRouteSettings={ThrottlingRateLimit: 1.0, ThrottlingBurstLimit: 1}).
4. Read ApiEndpoint from the CreateApi response: observe `https://<apiId>.execute-api.<region>.localhost:<port>`.
5. GET <ApiEndpoint>/ . Expected: the emulator's HTTP API data plane answers (200 from the backend, or 429 {"message":"Too Many Requests"} under a 40-way concurrent burst, then 200 again once the bucket refills). Actual: the address is not one the emulator is reached on, so the call is refused before it is made.
6. Equivalent: re-replay check apigateway__apigatewayv2.httpapi-stage-default-throttle-returns-default-too-many-requests.
