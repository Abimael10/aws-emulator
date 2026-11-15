Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-export-import-reimport-roundtrip
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-export-import-reimport-roundtrip apigatewayv2-import-export-reimport-api-round-trip apigatewayv2-import-export-reimport-api
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|ExportApi|2xx->2xx|$.info.version
Issue-Key: apigateway|ExportApi|2xx->2xx|$.servers[*].url
Issue-Key: apigateway|ExportApi|2xx->2xx|$.servers[*].variables
Issue-Key: apigateway|ExportApi|2xx->2xx|$.x-amazon-apigateway-importexport-version
Issue-Key: apigateway|ExportApi|4xx->4xx|$.message
Issue-Commit: 8efde9a8b1e68ca87350df1cc91c4a26e3648255

# ExportApi document and invalid-Specification refusal diverge from AWS

> _Note: this issue was identified against the code base at commit `8efde9a8b1e68ca87350df1cc91c4a26e3648255`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExportApi

## Description
Three net-new causes in API Gateway v2 ExportApi (GET /v2/apis/{ApiId}/exports/{Specification}). Smithy: ExportApi input ApiId/Specification @httpLabel required, OutputType required, ExportVersion/IncludeExtensions/StageName @httpQuery; output is a single blob member bound with @httpPayload (shape ExportedApi), so the model says nothing about the document's internal structure — the OpenAPI body below is recorded behavior only. Modeled errors: BadRequestException (400), NotFoundException (404), TooManyRequestsException (429).

Cause 1 of 3 — the exported OAS30 document omits the AWS import/export extension and the server's path variables.
AWS: recorded behavior only. A successful export of an HTTP API returns an OpenAPI 3.0 document that carries the top-level extension key `x-amazon-apigateway-importexport-version` with value `1.0`, and whose `servers[0]` object declares the base-path template variable, i.e. `variables: {"basePath": {"default": ""}}` alongside the server URL that references `{basePath}`. Both members are present in every recorded export in the corpus.
Emulator: both are absent from the serialized document — `$.x-amazon-apigateway-importexport-version` is missing (expected '1.0', got None) and `$.servers[0].variables` is missing (expected {'basePath': {'default': ''}}, got None). Status is 200 in both the recording and the emulator, so only the body diverges.
Why: a client that exports an API and feeds the document back to ImportApi/ReimportApi, or to any OpenAPI tool that resolves server variables, loses the base-path template: `{basePath}` in the server URL has no declared variable to substitute, and tooling that keys off `x-amazon-apigateway-importexport-version` to recognise an API Gateway export treats the document as a foreign OpenAPI file. The export/re-import round trip the fixtures exercise is therefore not faithful.
Fix: the ExportApi handler's OpenAPI document serializer — it must emit the extension key and the server-variables object when building the export body (the same code path that builds `servers`).
Traces: apigatewayv2-export-import-reimport-roundtrip#task#3, apigatewayv2-import-export-reimport-api-round-trip#task#4.

Cause 2 of 3 — the exported document's `servers[0].url` and `info.version` are emulator-internal values rather than the API's published form.
AWS: recorded behavior only. `servers[0].url` in an exported HTTP API document is the API's own execute-api endpoint with the base-path variable appended — the form `https://<apiId>.execute-api.<region>.amazonaws.com/{basePath}`, built from the request's ApiId and the service's region. `info.version` is a version string for the exported API, not a sub-second wall-clock stamp (the recordings carry a value of a different shape than the emulator's).
Emulator: `servers[0].url` is published as the emulator's own loopback listener plus an internal routing prefix, of the form `https://<host>:<port>/_aws/execute-api/<apiId>` — reported as `$.servers[0].url: resource_not_identified` / `value_mismatch` against the recorded `https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}` (the host, port and api id are per-run values, not the requirement; the requirement is the execute-api hostname form and the trailing `/{basePath}`). `info.version` is emitted as a microsecond-precision UTC instant (`$.info.version: value_mismatch`, actual of the form `2026-11-15T13:16:15.118962Z`), which does not match the shape the recording asserts. Hypothesis: the serializer reuses the emulator's data-listener address and `now()` instead of deriving the API endpoint and a version string from the API resource.
Why: the exported document describes an endpoint that is not the API's address, so any client, SDK or codegen tool that consumes the export targets the wrong host and loses the `{basePath}` substitution point; a version string that changes on every export also makes exports non-comparable. The open issue 20261115T090135Z-apigateway-apiendpoint-is-published-as-a-loopback-host.md covers the same loopback form on the CreateApi/ImportApi `apiEndpoint` member, but it does not cover the ExportApi document body, which is serialized separately.
Fix: the ExportApi handler's document serializer and the endpoint-formatting helper it uses for `servers` / `info`.
Traces: apigatewayv2-export-import-reimport-roundtrip#task#3, apigatewayv2-import-export-reimport-api-round-trip#task#4.

Cause 3 of 3 — the BadRequestException message for an unsupported Specification is emulator prose, not AWS's wording.
AWS: ExportApi with a Specification other than OAS30 is refused 400 BadRequestException with the message `Invalid export type detected. Currently, the only specification version is OAS30` (Smithy: BadRequestException, httpError 400, message serialized as `$.message`). That text is fixed for this error class — it contains no value taken from the request, so nothing in it varies.
Emulator: answers the correct status 400, but with the message `Invalid value for Specification: 'BOGUS'. Valid values are: OAS30.` — its own validation template, whose only variable part is the rejected Specification value echoed from the request path. Status and error class match; only the message text diverges.
Why: clients and test suites that match on the documented message text (or surface it to users) see a string AWS never produces, so error handling keyed to AWS's wording does not fire.
Fix: the ExportApi handler's Specification validation — raise BadRequestException with AWS's fixed message text instead of the generic invalid-value template.
Traces: apigatewayv2-import-export-reimport-api#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (107 fixtures, 908 steps, score 83.0%). Three unowned ExportApi clusters:

1) apigatewayv2-export-import-reimport-roundtrip#task#3 — ExportApi, expected 200, actual 200:
   $.x-amazon-apigateway-importexport-version: missing (expected '1.0', got None)
   $.servers[0].variables: missing (expected {'basePath': {'default': ''}}, got None)
   $.servers[0].url: resource_not_identified (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}', got 'https://127.0.0.1:35807/_aws/execute-api/mp67t4dbd4')
   $.info.version: value_mismatch (expected '<shaped-not-asserted>', got '2026-11-15T13:16:15.118962Z')

2) apigatewayv2-import-export-reimport-api-round-trip#task#4 — ExportApi, expected 200, actual 200:
   $.servers[0].url: value_mismatch (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com/{basePath}', got 'https://127.0.0.1:46699/_aws/execute-api/mp67t4dbd4')
   $.servers[0].variables: missing (expected {'basePath': {'default': ''}}, got None)
   $.x-amazon-apigateway-importexport-version: missing (expected '1.0', got None)

3) apigatewayv2-import-export-reimport-api#task#3 — ExportApi, expected 400, actual 400:
   $.message: value_mismatch (expected 'Invalid export type detected. Currently, the only specification version is OAS30', got "Invalid value for Specification: 'BOGUS'. Valid values are: OAS30.")

Hosts, ports, generated api ids and timestamps above are per-run values; the requirements are the presence of the extension key and server variables, the execute-api `https://<apiId>.execute-api.<region>.amazonaws.com/{basePath}` URL form, and the fixed 400 message text.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Causes 1 and 2: replay apigatewayv2-export-import-reimport-roundtrip step task#3 and apigatewayv2-import-export-reimport-api-round-trip step task#4 — create or import an HTTP API, then GET /v2/apis/{ApiId}/exports/OAS30?outputType=JSON&includeExtensions=true and inspect the returned document: `x-amazon-apigateway-importexport-version` and `servers[0].variables` are absent, `servers[0].url` is the emulator loopback listener instead of the API's execute-api endpoint with `/{basePath}`, and `info.version` is a microsecond wall-clock instant.
3. Cause 3: replay apigatewayv2-import-export-reimport-api step task#3 — GET /v2/apis/{ApiId}/exports/BOGUS?outputType=JSON and compare the 400 body's `message` to AWS's 'Invalid export type detected. Currently, the only specification version is OAS30'.
