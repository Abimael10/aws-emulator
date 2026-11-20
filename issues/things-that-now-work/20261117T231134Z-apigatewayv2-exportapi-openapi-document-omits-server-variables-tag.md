Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|ExportApi|2xx->2xx|$.info.version
Issue-Key: apigatewayv2|ExportApi|2xx->2xx|$.servers[*].url
Issue-Key: apigatewayv2|ExportApi|2xx->2xx|$.servers[*].variables
Issue-Key: apigatewayv2|ExportApi|2xx->2xx|$.tags
Issue-Key: apigatewayv2|ExportApi|2xx->2xx|$.x-amazon-apigateway-importexport-version
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigatewayv2 ExportApi OpenAPI document omits server variables, tag extensions and the importexport version

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExportApi

## Description
This bundle carries 1 cause.

Cause 1 of 1 — the OpenAPI document ExportApi returns for an HTTP API is missing three top-level/derived pieces and reports a stale info.version.
AWS: Smithy apigatewayv2#ExportApi (GET /v2/apis/{ApiId}/exports/{Specification}, http code 200) returns the document as the httpPayload blob ExportedApi. Recorded behavior for an HTTP API exported with extensions: the document carries (a) servers[0].url = the API's invoke URL with the '/{basePath}' template appended, (b) servers[0].variables = {"basePath": {"default": ""}}, (c) a top-level tags array rendering each API tag as {"name": <tag key>, "x-amazon-apigateway-tag-value": <tag value>}, and (d) the top-level extension "x-amazon-apigateway-importexport-version": "1.0". The member names, the x-amazon-apigateway-* extension spellings and the literal "1.0" are fixed; the tag key/value and the api id inside the URL come from the request and from state. info.version must follow the API's current version (the recording's step has that version cleared).
Emulator: servers[0].variables, the tags array and x-amazon-apigateway-importexport-version are absent from the exported document; servers[0].url is the emulator's loopback authority with an /_aws/execute-api/<apiId> path and no '/{basePath}' template; info.version still reports the pre-clear value 'v1'.
Why: the export is meant to be re-importable and tool-readable — a consumer resolving the basePath server variable, reading the API's tags out of the document, or branching on the importexport version gets nothing, and the server entry has no templated basePath to substitute. The stale info.version makes the document disagree with GetApi.
Fix: the ExportApi document builder (serializer) in the apigatewayv2 export handler — emit the servers[].variables basePath block, the x-amazon-apigateway-tag-value tags array and the x-amazon-apigateway-importexport-version extension, and take info.version from current API state. The loopback host:port in servers[0].url is this environment's address and not itself a requirement, but the missing '/{basePath}' suffix and the absent variables block are; the related endpoint-shape defect is tracked for the apigateway token in 20261115T090135Z-apigateway-apiendpoint-is-published-as-a-loopback-host.md, and a REST-side export divergence in 20261115T131901Z-apigateway-exportapi-document-and-invalid-specification-refusal-diverge-from.md — neither carries this step's replay key.
Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#6.

## Evidence
apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#6, ExportApi: 200 vs 200 with these diffs — $.info.version value_mismatch (expected a shaped value, got 'v1', the value the preceding UpdateApi should have cleared); $.servers[0].url value_mismatch (expected the API invoke URL ending in '/{basePath}', got 'https://127.0.0.1:<port>/_aws/execute-api/<apiId>'); $.servers[0].variables missing (expected {'basePath': {'default': ''}}); $.tags missing (expected [{'name': <tag key>, 'x-amazon-apigateway-tag-value': <tag value>}]); $.x-amazon-apigateway-importexport-version missing (expected '1.0').

## Steps to Reproduce
1. Replay fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags.
2. Compare step task#6 — GET /v2/apis/{apiId}/exports/OAS30 with extensions included — against the recorded document: expect servers[0].variables.basePath.default = '', a top-level tags array using x-amazon-apigateway-tag-value, x-amazon-apigateway-importexport-version = '1.0', a servers[0].url ending in '/{basePath}', and info.version following current API state; observe all three members absent, a loopback /_aws/execute-api URL, and info.version 'v1'.
