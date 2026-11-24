Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-api-mapping-negative-paths
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-api-mapping-negative-paths apigatewayv2-api-tagging-lifecycle apigatewayv2-reset-authorizers-cache apigatewayv2-route-request-parameter-lifecycle apigatewayv2-route-settings-lifecycle apigatewayv2-routing-rule-error-paths apigatewayv2-stage-access-log-settings-lifecycle apigatewayv2-stage-deployment-workflow apigatewayv2-tag-resource-workflow apigatewayv2-import-export-reimport-api apigatewayv2-import-export-reimport-api-round-trip
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|CreateApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigateway|ImportApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigateway|ReimportApi|2xx->2xx|$.apiEndpoint
Issue-Commit: 99f9186547f1799cdeb9842b49f33a85d7bdb1e3

# apigateway: apiEndpoint is published as a loopback host:port URL with an /_aws/execute-api path instead of AWS's {api-id}.execute-api.{region}.amazonaws.com form

> _Note: this issue was identified against the code base at commit `99f9186547f1799cdeb9842b49f33a85d7bdb1e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
3 causes, one per operation that mints the HTTP/WebSocket API invoke address. All three share one root cause: the `apiEndpoint` serializer emits the emulator's own data listener as a bare host:port plus a synthetic `/_aws/execute-api/<apiId>` path prefix, instead of the subdomain form AWS documents for that member. Every API-creating operation that returns an `Api` representation is affected.

Cause 1 of 3: CreateApi.
AWS: Smithy apigatewayv2 CreateApiResponse.apiEndpoint (target `__string`, jsonName `apiEndpoint`) is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." So the value is a URI whose AUTHORITY is `<apiId>.execute-api.<region>.<dnsSuffix>` — the api id is a DNS label of the authority, there is no port, and there is no path component before the stage name, so that appending `/<stageName>/<routePath>` yields the complete invoke path. The recorded step asserts the member's shape (expected `<shaped-not-asserted>`), i.e. the host FORM is the requirement; the concrete api id, region and port in any one recording are not.
Emulator: CreateApi answers 201 (status matches) but serializes apiEndpoint as `https://<host>:<port>/_aws/execute-api/<apiId>` — the replayed run returned `https://127.0.0.1:40171/_aws/execute-api/mp67t4dbd4`. The api id is demoted from a DNS label to a trailing PATH segment, an explicit port is present, and a two-segment `/_aws/execute-api` prefix is inserted before it.
Why: a client builds the invoke URL exactly as AWS documents — read apiEndpoint, append the stage name and route path. Against this value that yields `.../_aws/execute-api/<apiId>/<stage>/<route>`, and anything that parses the member instead of concatenating it is worse off: the api id is not the first DNS label, `urlsplit(...).hostname` is the emulator host rather than `<apiId>.execute-api...`, the region cannot be recovered from the authority, and custom-domain/api-mapping code that compares a stage's default domain against `<apiId>.execute-api.<region>.<dnsSuffix>` never matches. The port in the authority also breaks any client that assumes the default scheme port for an execute-api address.
Fix: the apigatewayv2 CreateApi handler's response serializer — specifically the member that derives `apiEndpoint` from the API's id and the server's bound address — must mint the documented subdomain authority rather than concatenating the listener address with a path prefix. The same derivation is shared by GetApi/GetApis/UpdateApi and by causes 2 and 3, so it belongs in the one place the endpoint string is composed, not per handler.
Traces: apigatewayv2-api-mapping-negative-paths#setup#1 (representative). The same cluster covers apigatewayv2-api-tagging-lifecycle#setup#1, apigatewayv2-reset-authorizers-cache#setup#1, apigatewayv2-route-request-parameter-lifecycle#setup#1, apigatewayv2-route-settings-lifecycle#task#1, apigatewayv2-routing-rule-error-paths#setup#1, apigatewayv2-stage-access-log-settings-lifecycle#setup#1, apigatewayv2-stage-deployment-workflow#task#1 and apigatewayv2-tag-resource-workflow#setup#1 — 9 steps in total.

Cause 2 of 3: ImportApi.
AWS: Smithy apigatewayv2 ImportApiResponse.apiEndpoint is the same `__string` member with the same documented form, so an API created by importing an OpenAPI body publishes an invoke URI of the form `https://{api-id}.execute-api.{region}.amazonaws.com`. The recorded response asserts that form concretely, with the freshly minted api id substituted into the first DNS label.
Emulator: ImportApi answers 200 (status matches the recording) but returns `https://127.0.0.1:34471/_aws/execute-api/mp67t4dbd4` — the host:port+path form of cause 1. The replay classifies it `resource_not_identified`: the api id the import minted cannot be located in the authority at all, because it is in the path.
Why: an import-then-invoke flow is the normal way an OpenAPI definition is deployed. A client that imports a definition and then resolves the new API by its endpoint's leading DNS label — the only identifier AWS puts there — cannot correlate the returned endpoint with the returned apiId, so it cannot address the imported API's stages.
Fix: the apigatewayv2 ImportApi handler's response serializer must compose apiEndpoint through the same corrected derivation as CreateApi, using the api id minted by the import.
Traces: apigatewayv2-import-export-reimport-api#task#1.

Cause 3 of 3: ReimportApi.
AWS: Smithy apigatewayv2 ReimportApiResponse.apiEndpoint is again the same member and form. Re-importing a body into an existing API keeps the API's identity, so the published endpoint keeps the SAME authority it had before the re-import — `https://<that api's id>.execute-api.<region>.amazonaws.com`. The recorded response asserts exactly that, with the pre-existing api id in the leading label.
Emulator: ReimportApi answers 200 (status matches) and returns `https://127.0.0.1:46211/_aws/execute-api/mp67t4dbd4` — again host:port plus `/_aws/execute-api/<apiId>`.
Why: a re-import round trip is used to verify that re-importing did not move the API. Because the authority carries the emulator's listener and not the api id, the endpoint a client holds from before the re-import and the one returned after it compare as equal only by accident (both are the listener), and differ across runs by port, so the invariant "re-import preserves the invoke address" cannot be checked, and the id-bearing portion of the address is in the wrong component.
Fix: the apigatewayv2 ReimportApi handler's response serializer, via the same shared apiEndpoint derivation.
Traces: apigatewayv2-import-export-reimport-api-round-trip#task#5.

Relationship to open issues (neither covers these three wire steps; the replay reports no owner for them): 20261114T171242Z-apigateway-apigatewayv2-reopens-an-http-api-s-published-apiendpoint-is.md and 20261109T143515Z-apigateway-dynamodb-lambda-logs-http-api-apiendpoint-is-published-as-id-execute.md are BEHAVIORAL issues filed from the opposite direction: they report that apiEndpoint was an `<apiId>.execute-api.<region>.amazonaws.com` host the emulator did not SERVE, and ask for a reachable address. The current value is reachable but no longer AWS-shaped, which is what these wire steps catch. Satisfying both requires the documented authority to be served — i.e. the execute-api data plane must answer for `<apiId>.execute-api.<region>.<dnsSuffix>` (e.g. by Host-header routing on the emulator's own listener) — not a rollback to either half.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false).

Cause 1 — CreateApi, fixture apigatewayv2-api-mapping-negative-paths, step setup#1 (cluster of 9 steps):
  status: expected 201, got 201 (matches)
  $.apiEndpoint: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:40171/_aws/execute-api/mp67t4dbd4')
  Expected form: https://<apiId>.execute-api.<region>.<dnsSuffix> — api id as the leading DNS label, no port, no path.
  Actual form:   https://<emulator host>:<port>/_aws/execute-api/<apiId>.

Cause 2 — ImportApi, fixture apigatewayv2-import-export-reimport-api, step task#1:
  status: expected 200, got 200 (matches)
  $.apiEndpoint: resource_not_identified (expected 'https://{{imported_api_id}}.execute-api.us-west-2.amazonaws.com', got 'https://127.0.0.1:34471/_aws/execute-api/mp67t4dbd4')
  The expected value is templated on the api id the import minted; the actual value carries no api id in its authority.

Cause 3 — ReimportApi, fixture apigatewayv2-import-export-reimport-api-round-trip, step task#5:
  status: expected 200, got 200 (matches)
  $.apiEndpoint: value_mismatch (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://127.0.0.1:46211/_aws/execute-api/mp67t4dbd4')

In all three, the concrete api id (mp67t4dbd4), the region (us-west-2) and the ports (40171 / 34471 / 46211) are per-run values and are NOT part of the requirement. The defect is the URI's structure: which component carries the api id, the presence of an explicit port, and the injected `/_aws/execute-api` path prefix.

Model citation: apigatewayv2 Smithy — Api$ApiEndpoint documentation "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage."; member present on CreateApiResponse, ImportApiResponse and ReimportApiResponse as jsonName `apiEndpoint`.

## Steps to Reproduce
Cause 1 (CreateApi):
1. POST /v2/apis with {"name": "<any>", "protocolType": "HTTP"}.
2. Read `apiEndpoint` from the 201 body.
Expected: a URI whose authority is `<apiId>.execute-api.<region>.<dnsSuffix>` with no port and no path, so that appending `/<stageName>/<routePath>` is the complete invoke path.
Actual: `https://<emulator host>:<port>/_aws/execute-api/<apiId>` — api id in the path, explicit port, `/_aws/execute-api` prefix.

Cause 2 (ImportApi):
1. PUT /v2/apis with an OpenAPI 3 body defining one HTTP API with a route.
2. Read `apiId` and `apiEndpoint` from the response.
Expected: `apiEndpoint`'s leading DNS label equals the returned `apiId`.
Actual: the authority is the emulator's host:port; the api id appears only as the final path segment, so it cannot be recovered from the authority.

Cause 3 (ReimportApi):
1. Create or import an HTTP API and record its `apiId` and `apiEndpoint`.
2. PUT /v2/apis/{apiId} with a modified OpenAPI body.
3. Read `apiEndpoint` from the response.
Expected: the same documented authority as before the re-import, `https://<apiId>.execute-api.<region>.<dnsSuffix>`.
Actual: `https://<emulator host>:<port>/_aws/execute-api/<apiId>`.

Replay the three bound steps to confirm: apigatewayv2-api-mapping-negative-paths#setup#1, apigatewayv2-import-export-reimport-api#task#1, apigatewayv2-import-export-reimport-api-round-trip#task#5.
