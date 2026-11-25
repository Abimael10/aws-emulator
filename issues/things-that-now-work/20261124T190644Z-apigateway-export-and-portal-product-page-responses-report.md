Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-import-export-reimport-api-round-trip
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-import-export-reimport-api-round-trip apigatewayv2-product-rest-endpoint-page-lifecycle
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|ExportApi|2xx->2xx|$.servers[*].url
Issue-Key: apigateway|GetProductRestEndpointPage|2xx->2xx|$.status
Issue-Key: apigateway|GetProductRestEndpointPage|2xx->2xx|$.statusException
Issue-Commit: ebf3a28cf5dce567e4c0f686d2c323b3753a6406

# apigateway export and portal-product-page responses report emulator-local values instead of the ones AWS computes

> _Note: this issue was identified against the code base at commit `ebf3a28cf5dce567e4c0f686d2c323b3753a6406`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExportApi

## Description
Two net-new causes on the apigateway surface. Both are response BODY fields the emulator fills from its own local deployment/state shortcut rather than computing the value AWS reports, so the response is well-formed but carries a value a client cannot use. BOTH causes must be closed before this issue is submitted.

Cause 1 of 2 — ExportApi emits a loopback `servers[0].url` in the exported OpenAPI document (fixture apigatewayv2-import-export-reimport-api-round-trip, step task#4).
AWS: Smithy apigatewayv2 ExportApi — GET /v2/apis/{ApiId}/exports/{Specification}, output is the `ExportedApi` blob httpPayload, 200. The exported OpenAPI 3 document's `servers[0].url` is the API's default invoke URL, i.e. the scheme+host form `https://{api-id}.execute-api.{region}.amazonaws.com` with the mapping's base path appended as the `/{basePath}` server-variable segment when the export is taken for a stage/mapping. The host is a DNS name whose FIRST label is the API id and whose remaining labels are `execute-api.{region}.amazonaws.com`; there is no port and no `/_aws/...` prefix segment. Recorded response shows `https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}` (the `mp67t4dbd4` label is the request's API id and `us-west-2` the request's region — both are values from the request/state; the fixed text is the `.execute-api.`/`.amazonaws.com` suffix, the `https://` scheme and the `/{basePath}` variable segment).
Emulator: returns `https://127.0.0.1:39307/_aws/execute-api/mp67t4dbd4/{basePath}` — a host:port loopback authority with the API id demoted to a path segment behind an `/_aws/execute-api` prefix. The API id is not the host's first label, so the exported document does not describe an execute-api endpoint at all.
Why: the export is the artifact clients hand to SDK/codegen/import tooling. A consumer that generates a client, re-imports the spec elsewhere, or validates the `servers` entry against the `{api-id}.execute-api.{region}.amazonaws.com` pattern gets a document pointing at a non-routable loopback authority, so generated clients target the wrong host and spec-shape assertions fail.
Fix: the ExportApi handler's OpenAPI document builder — wherever the `servers` list is synthesized — must render the API's canonical invoke URL from the api id + region, the same way the apiEndpoint member should be rendered, instead of echoing the local listener authority. This is the SAME underlying endpoint-URL construction reported for GetApi/ReimportApi in open issue 20261115T090135Z-apigateway-apiendpoint-is-published-as-a-loopback-host.md, but that issue's replay key does not cover the ExportApi step (the replay reports this cluster as unowned), so the export path must be fixed and re-replayed here.
Traces: apigatewayv2-import-export-reimport-api-round-trip#task#4 (ExportApi, 200 vs 200, key $.servers[0].url).

Cause 2 of 2 — GetProductRestEndpointPage always reports Status AVAILABLE and never populates StatusException (fixture apigatewayv2-product-rest-endpoint-page-lifecycle, steps task#7 and task#9).
AWS: Smithy apigatewayv2 GetProductRestEndpointPage — GET /v2/portalproducts/{PortalProductId}/productrestendpointpages/{ProductRestEndpointPageId}, 200, output members `status` (enum Status, values AVAILABLE | IN_PROGRESS | FAILED) and `statusException` (structure StatusException with `exception` @length 1..256 and `message` @length 1..2048, documented as "The exception" / "The error message"). A product REST endpoint page is validated against the `restEndpointIdentifier` it was created with; when that referenced REST endpoint cannot be resolved the page settles on the terminal value `FAILED` and the response carries `statusException` with both members populated. Recorded response: `status` = "FAILED" with a `statusException` object present (its `exception`/`message` text is shaped-not-asserted in the fixture, so the requirement is that the member is PRESENT and populated, not any one recording's wording). The page's own id/arn/timestamps are per-run values and are not part of the requirement.
Emulator: answers 200 with `status` = "AVAILABLE" and omits `statusException` entirely, on both reads of the page.
Why: AVAILABLE is a terminal success value, so a client that polls this page to learn whether its endpoint reference resolved is told the page is serviceable when AWS says it failed, and it gets no `statusException` to explain why. Failure is silently reported as success, and any consumer branching on FAILED (or reading statusException.message to surface the reason) takes the wrong branch forever.
Fix: the portal-product REST-endpoint-page state/handler — the page's status must be derived from validating the stored `restEndpointIdentifier` (producing FAILED plus a populated StatusException when it does not resolve, IN_PROGRESS while validating) instead of being hard-set to AVAILABLE at create time, and GetProductRestEndpointPage's serializer must emit `statusException` whenever the page is in the FAILED state.
Traces: apigatewayv2-product-rest-endpoint-page-lifecycle#task#7 and #task#9 (GetProductRestEndpointPage, 200 vs 200, keys $.status, $.statusException).

## Evidence
Cause 1 — ExportApi, fixture apigatewayv2-import-export-reimport-api-round-trip step task#4, expected status 200, actual 200:
  $.servers[0].url: value_mismatch
    expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}'
    actual   'https://127.0.0.1:39307/_aws/execute-api/mp67t4dbd4/{basePath}'
  (requirement is the FORM: https://{api-id}.execute-api.{region}.amazonaws.com + /{basePath}; the id and region shown are this run's request values.)

Cause 2 — GetProductRestEndpointPage, fixture apigatewayv2-product-rest-endpoint-page-lifecycle steps task#7 and task#9, expected status 200, actual 200:
  $.status: value_mismatch (expected 'FAILED', got 'AVAILABLE')
  $.statusException: missing (expected {'exception': <present/populated>, 'message': <present/populated>}, got None)
  Smithy: Status enum = AVAILABLE | IN_PROGRESS | FAILED; StatusException{exception @length 1..256, message @length 1..2048}.

Replay summary for this corpus: trace_set simparity-wire-aws-v1, emulator_up true, 1057 steps, 934 passed, score 88.4%. Both clusters above are reported with empty owned_by and empty stub_path, i.e. not covered by any open issue.

## Steps to Reproduce
Cause 1:
1. Boot the emulator from this checkout and replay fixture `apigatewayv2-import-export-reimport-api-round-trip`.
2. At step `task#4` the harness calls ExportApi (GET /v2/apis/{ApiId}/exports/oas30) for the API created in the fixture's setup.
3. Parse the returned OpenAPI document and read `servers[0].url`. Expect a URL whose host is `{api-id}.execute-api.{region}.amazonaws.com` with no port and no `/_aws/execute-api/...` prefix; the emulator returns the loopback `host:port` + path-style form.

Cause 2:
1. Replay fixture `apigatewayv2-product-rest-endpoint-page-lifecycle`.
2. The fixture creates a portal product and a product REST endpoint page via CreateProductRestEndpointPage with a restEndpointIdentifier that does not resolve.
3. At steps `task#7` and `task#9` it calls GetProductRestEndpointPage. Expect `status` = "FAILED" and a populated `statusException` object; the emulator answers `status` = "AVAILABLE" with no `statusException` member.
