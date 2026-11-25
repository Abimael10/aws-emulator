Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-api-mapping-negative-paths
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-api-mapping-negative-paths apigatewayv2-import-export-reimport-api-round-trip
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|CreateApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigateway|ReimportApi|2xx->2xx|$.apiEndpoint
Issue-Commit: bffc3dc208134b3f93be634b9f8f0386478534eb

# apigateway CreateApi and ReimportApi publish apiEndpoint as a path-style loopback URL, not the {api-id}.execute-api.{region} host form

> _Note: this issue was identified against the code base at commit `bffc3dc208134b3f93be634b9f8f0386478534eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Two net-new causes, one shared fault: every write path that returns an Api object derives the response's `apiEndpoint` from the emulator's own data listener, as a path-style URL that carries the API id in the PATH, instead of the documented host form in which the id, the literal label `execute-api` and the region are the leading labels of the hostname. The two causes below are two separate operations building the same field wrongly; each must be closed.

Cause 1 of 2 — CreateApi (fixture apigatewayv2-api-mapping-negative-paths, step setup#1).
AWS: Smithy apigatewayv2 CreateApi -> CreateApiResponse.ApiEndpoint (jsonName `apiEndpoint`, target __string), HTTP POST /v2/apis with success code 201. The member is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." The requirement is therefore the URI FORM: the value is an absolute `https://` URI whose hostname's three leading dot-separated labels are, in order, the newly created API's own id (the same value returned in `apiId`), the literal label `execute-api`, and the region the API was created in; the stage/route path is appended after that host. The DNS suffix after those labels and the port are environment values and are not part of the requirement. The recorded response status is 201 and no error is involved — the status matched, only this field diverged.
Emulator: CreateApi answers 201 but sets `apiEndpoint` to a path-style URL on its own service listener, of the form `https://<emulator-host>:<port>/_aws/execute-api/<api-id>` (the replayed step reports `$.apiEndpoint: value_mismatch`, actual `https://127.0.0.1:46201/_aws/execute-api/mp67t4dbd4`; host, port and api id are per-run values, the URL form is the defect). Split on '.', that hostname's leading labels are the octets of the loopback address rather than (<api-id>, `execute-api`, <region>), and neither `execute-api` nor the region appears in the host at all.

Cause 2 of 2 — ReimportApi (fixture apigatewayv2-import-export-reimport-api-round-trip, step task#5).
AWS: Smithy apigatewayv2 ReimportApi -> ReimportApiResponse.ApiEndpoint (jsonName `apiEndpoint`, target __string), HTTP PUT /v2/apis/{ApiId}. ReimportApi returns the same Api view as CreateApi, so `apiEndpoint` carries the identical documented form, and reimporting an existing API must not change it: the value is the endpoint of the API named by the path label `{ApiId}`, i.e. host leading labels (<that api id>, `execute-api`, <region>). The recorded response for this step is status 200 with `apiEndpoint` of exactly that shape — `https://<api-id>.execute-api.<region>.amazonaws.com` (one run: `https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com`; the id and region are values taken from the request and state, the form is the requirement).
Emulator: ReimportApi answers 200 but returns the same path-style loopback URL, `https://<emulator-host>:<port>/_aws/execute-api/<api-id>` (replayed actual `https://127.0.0.1:34525/_aws/execute-api/mp67t4dbd4`), so a round trip of import -> export -> reimport never restores the documented endpoint form.

Why: a client that follows AWS's documented contract parses `apiEndpoint` to recover the API's identity and region — SigV4 signing of execute-api data-plane requests derives the signing region and service from the host, per-API host routing and logging/metric dimensions take the api id from the leading label, and SDK/CDK code builds `<apiEndpoint>/<stage>/<route>`. Against the emulator all of those read the loopback octets: they extract `127` as the api id, find no `execute-api` label and have no region, so request signing and host-based routing of the execute-api plane cannot work and every API appears to share one hostname. Because ReimportApi diverges too, a client cannot recover the correct endpoint by re-importing.

Fix: the shared apigatewayv2 Api-endpoint derivation used when serializing the Api view — the one feeding CreateApi's and ReimportApi's response serializers (and the same helper the Import/Get paths reuse) — must build the endpoint as a host whose leading labels are the api id, `execute-api` and the region, on a suffix and port the emulator's execute-api data plane actually serves, instead of embedding the api id in a `/_aws/execute-api/<id>` path on the service listener.

Related but NOT covering: open issue 20261118T125928Z-apigatewayv2-cloudcontrol-cloudformation-apigatewayv2-publishes-getapi-fn-getatt-apiendpoint-as-a.md describes the same URL-form fault for GetApi / Fn::GetAtt ApiEndpoint under service `apigatewayv2`; it carries no replay key for either fixture step here, and the replay reports both clusters as unowned, so CreateApi's and ReimportApi's serializers are still unfixed and unexercised by it. Open issue 20261124T190644Z-apigateway-export-and-portal-product-page-responses-report.md owns the matching ExportApi `servers[0].url` divergence and is excluded from this bundle.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, 1057 steps (935 passed, 122 mismatched), 54 divergence clusters. Both clusters below have empty `stub_path` and empty `owned_by`.

Cause 1 of 2 — fixture apigatewayv2-api-mapping-negative-paths, step setup#1, service apigateway, operation CreateApi, verdict mismatch:
  divergence_key: service=apigateway|CreateApi|status=201->201|keys=$.apiEndpoint
  status: expected 201, got 201 (status agrees)
  $.apiEndpoint: value_mismatch
    Expected: an execute-api URI whose host's leading labels are (<apiId from the same response>, 'execute-api', <region>) — recorded as shape-asserted.
    Actual:   'https://127.0.0.1:46201/_aws/execute-api/mp67t4dbd4' — hostname leading labels ('127','0','0'); api id moved into a '/_aws/execute-api/<id>' path.

Cause 2 of 2 — fixture apigatewayv2-import-export-reimport-api-round-trip, step task#5, service apigateway, operation ReimportApi, verdict mismatch:
  divergence_key: service=apigateway|ReimportApi|status=200->200|keys=$.apiEndpoint
  status: expected 200, got 200 (status agrees)
  $.apiEndpoint: value_mismatch
    Expected: 'https://<api-id>.execute-api.<region>.amazonaws.com' (recorded run: 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com'; the id and region come from the request/state, the form is the requirement).
    Actual:   'https://127.0.0.1:34525/_aws/execute-api/mp67t4dbd4'.

No error code or error message is involved in either cause: both responses carry the modeled success status, and only the apiEndpoint member's URI form diverges. Hosts, ports and the generated api id differ per run and are not asserted; the asserted requirement is the hostname's leading-label structure.

## Steps to Reproduce
Cause 1 of 2 (CreateApi):
1. Replay fixture apigatewayv2-api-mapping-negative-paths against the emulator and stop at step setup#1, or by hand: POST /v2/apis with body {"name": "<any name>", "protocolType": "HTTP"}.
2. Read the 201 response's `apiEndpoint`, take urlsplit(apiEndpoint).hostname and split it on '.'.
   Expected: the first three labels are (<the response's apiId>, 'execute-api', <region>), with no '/_aws/execute-api/<id>' path prefix.
   Actual: the host is the emulator's listener address, so the leading labels are the loopback octets ('127','0','0') and the api id appears only in the path.

Cause 2 of 2 (ReimportApi):
1. Replay fixture apigatewayv2-import-export-reimport-api-round-trip through step task#5, or by hand: create an HTTP API via ImportApi, call ExportApi, then PUT /v2/apis/{apiId} (ReimportApi) with that OpenAPI document as the body.
2. Read the 200 response's `apiEndpoint` and apply the same hostname check.
   Expected: 'https://<apiId>.execute-api.<region>.amazonaws.com' — host leading labels (<apiId>, 'execute-api', <region>).
   Actual: 'https://<emulator-host>:<port>/_aws/execute-api/<apiId>'.
