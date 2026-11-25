Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|ExportApi|2xx->2xx|$.servers[*].url
Issue-Commit: 1a552fe8708f5d6fbecf5907a9407a2fce831c57

# apigatewayv2 ExportApi advertises an execute-api host the emulator does not answer on

> _Note: this issue was identified against the code base at commit `1a552fe8708f5d6fbecf5907a9407a2fce831c57`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExportApi

## Description
One net-new cause on the `apigatewayv2` surface.

Cause 1 of 1 — the OpenAPI document ExportApi returns names an endpoint the emulator is not reachable on (fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#6).
AWS: Smithy apigatewayv2 ExportApi — GET /v2/apis/{ApiId}/exports/{Specification}, success code 200, output is the single `ExportedApi` blob bound as @httpPayload (input: ApiId @httpLabel required, Specification @httpLabel required, OutputType required, ExportVersion/IncludeExtensions/StageName @httpQuery; errors BadRequestException 400, NotFoundException 404, TooManyRequestsException 429). The model documents the payload only as "an exported definition of an API in a particular output format" and says nothing about the document's contents, so for the `servers` entry this is RECORDED BEHAVIOR ONLY: the recording shows `servers[0].url` is the endpoint at which that API is actually served, with the base-path server variable appended — `https://<host serving the API>/{basePath}` (one run: `https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}`; the api id and the region are values taken from the request and state, while the `https://` scheme and the `/{basePath}` variable segment are fixed). The requirement is that the advertised endpoint is one the serving host actually answers on; no single run's host string is itself the requirement.
Emulator: answers 200, and the exported document's `servers[0].url` is expanded from a hard-coded `https://{apiId}.execute-api.{region}.amazonaws.com/{basePath}` template — a public AWS DNS name this emulator does not serve. The replay verdict for `$.servers[0].url` is `host_not_own`: the emulator announced an address other than the one it is reached on. The status agrees (200 vs 200) and no error code or error message is involved; only this field diverges.
Why: the exported spec is what clients hand to code generators, spec importers, mock servers and HTTP clients. Built against the emulator's export, every generated client and every re-import targets the real AWS execute-api domain, so invoke traffic leaves the emulator (or fails to resolve) and never reaches the emulated API — the API appears exported successfully yet is unreachable through its own published spec.
Fix: the ExportApi handler's OpenAPI-document builder — the `servers[]` renderer — must derive the advertised authority from the endpoint the emulator actually serves this API on (the request's Host / the configured data listener and its execute-api route), instead of expanding a fixed `execute-api.<region>.amazonaws.com` template.
Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#6 (ExportApi, 200 vs 200, key $.servers[0].url, verdict host_not_own).

Related but NOT covering: the SAME renderer fault is reported for the clusters whose replayed `service` token is `apigateway` in the companion bundle filed this run (representative apigatewayv2-export-import-reimport-roundtrip#task#3); that bundle carries a different service token and therefore cannot carry this cluster's replay key. The resolved issue 20261124T190644Z-apigateway-export-and-portal-product-page-responses-report.md covered the earlier, opposite symptom (the export emitted a loopback `host:port` + `/_aws/execute-api/<id>` path URL); it is resolved and does not cover this cluster. Resolved issue 20261117T231134Z-apigatewayv2-exportapi-openapi-document-omits-server-variables-tag.md concerned missing server VARIABLES/extensions in the same document, not the advertised authority. Resolved issue 20261110T030315Z-apigateway-api-gateway-getexport-advertises-an-execute-api-host.md records the identical host_not_own fault in the REST GetExport document. The replay reports this cluster with empty owned_by and empty stub_path.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, 1057 steps (932 passed, 124 mismatched, 1 errored), 55 divergence clusters, score 88.2%. This cluster carries stub_path "" and owned_by [].

Fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#6, service apigatewayv2, operation ExportApi, verdict mismatch, affected_count 1:
  divergence_key: service=apigatewayv2|ExportApi|status=200->200|keys=$.servers[0].url
  status: expected 200, got 200 (status agrees; no error code or message involved)
  $.servers[0].url: host_not_own
    Expected: the exported document advertises the endpoint the API is actually served on — scheme + an authority the serving host answers on + '/{basePath}' (recorded run: 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}'; the api id and region are per-run request/state values).
    Actual:   'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}' — textually the recorded AWS host, but an address this emulator does not answer on, which is why the verdict is host_not_own rather than a value mismatch.
  Smithy: ExportApi GET /v2/apis/{ApiId}/exports/{Specification}, code 200, output ExportedApi (blob, @httpPayload); the model does not describe the document body, so the servers[] contract is recorded behavior only.

Hosts, ports, generated api ids and timestamps differ per run and are not requirements; the asserted requirement is that the exported endpoint is one the emulator answers on.

## Steps to Reproduce
1. git_clone(), then replay_wire_corpus(); find the cluster service=apigatewayv2|ExportApi|status=200->200|keys=$.servers[0].url (fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#6).
2. By hand: POST /v2/apis to create an HTTP API, then GET /v2/apis/{apiId}/exports/OAS30?outputType=JSON.
3. Parse the returned OpenAPI document and read servers[0].url.
   Expected: an authority the emulator answers on (the host the request arrived on, or the listener that serves this API's execute-api traffic), followed by '/{basePath}'.
   Actual: 'https://{apiId}.execute-api.{region}.amazonaws.com/{basePath}'.
4. Point any generated client, importer or curl at the advertised authority: it does not reach the emulator — it resolves to (or fails to resolve) public AWS.
