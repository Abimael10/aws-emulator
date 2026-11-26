Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags apigatewayv2-http-route-key-grammar apigatewayv2-sstr-traj-quick-create-then-solve-sh-steps apigatewayv2-import-reimport-status-codes apigatewayv2-api-stage-bogus-child-not-found apigatewayv2-get-apis-membership-and-page-round-trip
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|CreateApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigatewayv2|GetApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigatewayv2|UpdateApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigatewayv2|GetApis|2xx->2xx|$.items[*].apiEndpoint
Issue-Key: apigatewayv2|GetApis|2xx->2xx|$.items[name="sim2real-probe-apigwv2-http-fb2eac9e"].apiEndpoint
Issue-Key: apigatewayv2|ImportApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigatewayv2|ReimportApi|2xx->2xx|$.apiEndpoint
Issue-Commit: 4c0072743c4b802ee07d254916613d27e39cf370

# apigatewayv2 Api views publish apiEndpoint on an unserved execute-api .localhost host

> _Note: this issue was identified against the code base at commit `4c0072743c4b802ee07d254916613d27e39cf370`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
SEVEN net-new causes, one shared fault. Under the `apigatewayv2` service token, every operation that serializes an `Api` view derives `apiEndpoint` from the emulator's own data listener and appends a `.localhost:<port>` suffix to the three documented leading labels. The URI has the right leading-label structure but names a host that is NOT an address the emulator is reached on, so the replay classifies it `host_not_own`: the emulator announces an endpoint other than the one it answers on. The seven causes below are seven separate response serializers building the same field wrongly; each must be closed.

Shared AWS requirement: Smithy apigatewayv2, member doc on Api.ApiEndpoint - "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." The requirement is the URI form AND its reachability: an absolute URI whose hostname's leading dot-separated labels are, in order, the API's own id (the same value returned in `apiId`), the literal label `execute-api`, and the region, on a DNS suffix the execute-api data plane actually serves. For a WEBSOCKET-protocol API the recorded responses use the `wss://` scheme rather than `https://` (recorded behavior only - the model states no scheme). Generated api ids, DNS suffixes and ports are per-run environment values and are not part of the requirement.

Shared Why: a client following the documented contract uses `apiEndpoint` as the base URL of the deployed API, derives the SigV4 signing region and service from its host, and builds `<apiEndpoint>/<stage>/<route>`. Against the emulator that host resolves to nothing it answers on, so every data-plane call built from a successful control-plane read or write fails to connect, and a WEBSOCKET client additionally gets an `https://` URL where it must dial `wss://`.

Shared Fix: the single apigatewayv2 Api-endpoint derivation used when serializing the `Api` view - the helper feeding the Create/Get/Update/List/Import/Reimport response serializers - must build the endpoint on a host the emulator's execute-api data plane is actually reachable on while keeping the (<api-id>, `execute-api`, <region>) leading labels, and must select the scheme from the API's `protocolType` (wss for WEBSOCKET).

Cause 1 of 7 - CreateApi (fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#1). AWS: CreateApi -> CreateApiResponse.ApiEndpoint, HTTP POST /v2/apis, success code 201; recorded status 201, no error involved. Emulator: answers 201 but publishes `https://<api-id>.execute-api.<region>.localhost:<port>` (replay detail: `$.apiEndpoint: host_not_own (expected '<shaped-not-asserted>', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:40651')`). Traces: that fixture#task#1 (cluster of 3: also apigatewayv2-http-route-key-grammar#setup#1, apigatewayv2-sstr-traj-quick-create-then-solve-sh-steps#task#1).

Cause 2 of 7 - GetApi (same fixture, step task#2). AWS: GetApi -> GetApiResponse.ApiEndpoint, HTTP GET /v2/apis/{ApiId}, success code 200; recorded status 200. Emulator: answers 200 with the unserved host (replay detail: `$.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:40651')`). Traces: that fixture#task#2 (cluster of 3: also that fixture#task#4, apigatewayv2-import-reimport-status-codes#task#3).

Cause 3 of 7 - UpdateApi (fixture apigatewayv2-api-stage-bogus-child-not-found, step task#2). AWS: UpdateApi -> UpdateApiResponse.ApiEndpoint, HTTP PATCH /v2/apis/{ApiId}; recorded status 201. An update must not change the endpoint form - it stays the reachable execute-api host of the API named by `{ApiId}`. Emulator: answers 201 with the unserved host (replay detail: `$.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:38453')`). Traces: apigatewayv2-api-stage-bogus-child-not-found#task#2 (cluster of 2: also apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#3).

Cause 4 of 7 - GetApis, list page (fixture apigatewayv2-get-apis-membership-and-page-round-trip, step task#4). AWS: GetApis -> GetApisResponse.Items (jsonName `items`, target __listOfApi), HTTP GET /v2/apis, success code 200; each element is the same `Api` shape, so every `items[*].apiEndpoint` must be that element's own reachable execute-api endpoint, with the `wss://` scheme for a WEBSOCKET API (recorded behavior). Emulator: answers 200 but the element carries an `https://...localhost:<port>` host (replay detail: `$.items[0].apiEndpoint: host_not_own (expected 'wss://e77k44w5b0.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46155')`) - the host is unserved and the WEBSOCKET element's scheme is https rather than wss. Traces: apigatewayv2-get-apis-membership-and-page-round-trip#task#4.

Cause 5 of 7 - GetApis, membership read (same fixture, step task#5). Same AWS requirement as cause 4, asserted both positionally and by name. Emulator: answers 200 and both assertions diverge (replay detail: `$.items[name="sim2real-probe-apigwv2-http-fb2eac9e"].apiEndpoint: value_mismatch (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46155')` and `$.items[1].apiEndpoint: host_not_own (expected 'wss://e77k44w5b0.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46155')`), so a named-lookup client and a positional one both get the unserved host, and the WEBSOCKET entry again lacks the wss scheme. Traces: apigatewayv2-get-apis-membership-and-page-round-trip#task#5.

Cause 6 of 7 - ImportApi (fixture apigatewayv2-import-reimport-status-codes, step task#1). AWS: ImportApi -> ImportApiResponse.ApiEndpoint, HTTP PUT /v2/apis, modeled success code 201; the replayed step records 200. Emulator: answers 200 with the unserved host (replay detail: `$.apiEndpoint: host_not_own (expected 'https://90vho59q5k.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:36585')`). Traces: apigatewayv2-import-reimport-status-codes#task#1.

Cause 7 of 7 - ReimportApi (same fixture, step task#2). AWS: ReimportApi -> ReimportApiResponse.ApiEndpoint, HTTP PUT /v2/apis/{ApiId}, modeled success code 201; the replayed step records 200. Reimporting must not change the endpoint of the API named by `{ApiId}`. Emulator: answers 200 with the unserved host (replay detail: `$.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:36585')`), so an import -> export -> reimport round trip never restores a reachable endpoint. Traces: apigatewayv2-import-reimport-status-codes#task#2.

Related but NOT covering: open issue 20261126T134923Z-apigatewayv2-appconfig-iam-sts-apigatewayv2-createapi-publishes-apiendpoint-as-an-api-id.md and the 20261126T140544Z-* / 20261126T140244Z-* behavioural issues describe the same unserved `<api-id>.execute-api.<region>.localhost:<port>` host from the data-plane side, but none of them carries a replay key for any of the seven fixture steps above, and the replay reports all seven clusters as unowned; issue 20261123T055131Z-apigatewayv2-tag-plane-diverges-gettags-omits-tags-written.md owns the unrelated GetTags clusters in this corpus and is excluded from this bundle.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, 1057 steps (952 passed, 104 mismatched, 1 errored), 34 divergence clusters, score_pct 90.1. All seven clusters below have empty `stub_path` and empty `owned_by`.

Cause 1 of 7 - fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#1, service apigatewayv2, operation CreateApi, verdict mismatch:
  divergence_key: service=apigatewayv2|CreateApi|status=201->201|keys=$.apiEndpoint
  status: expected 201, got 201 (status agrees)
  $.apiEndpoint: host_not_own (expected '<shaped-not-asserted>', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:40651'); affected_count 3.

Cause 2 of 7 - same fixture, step task#2, operation GetApi, verdict mismatch:
  divergence_key: service=apigatewayv2|GetApi|status=200->200|keys=$.apiEndpoint
  status: expected 200, got 200
  $.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:40651'); affected_count 3.

Cause 3 of 7 - fixture apigatewayv2-api-stage-bogus-child-not-found, step task#2, operation UpdateApi, verdict mismatch:
  divergence_key: service=apigatewayv2|UpdateApi|status=201->201|keys=$.apiEndpoint
  status: expected 201, got 201
  $.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:38453'); affected_count 2.

Cause 4 of 7 - fixture apigatewayv2-get-apis-membership-and-page-round-trip, step task#4, operation GetApis, verdict mismatch:
  divergence_key: service=apigatewayv2|GetApis|status=200->200|keys=$.items[0].apiEndpoint
  status: expected 200, got 200
  $.items[0].apiEndpoint: host_not_own (expected 'wss://e77k44w5b0.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46155'); affected_count 1.

Cause 5 of 7 - same fixture, step task#5, operation GetApis, verdict mismatch:
  divergence_key: service=apigatewayv2|GetApis|status=200->200|keys=$.items[1].apiEndpoint,$.items[name="sim2real-probe-apigwv2-http-fb2eac9e"].apiEndpoint
  status: expected 200, got 200
  $.items[name="sim2real-probe-apigwv2-http-fb2eac9e"].apiEndpoint: value_mismatch (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46155')
  $.items[1].apiEndpoint: host_not_own (expected 'wss://e77k44w5b0.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46155'); affected_count 1.

Cause 6 of 7 - fixture apigatewayv2-import-reimport-status-codes, step task#1, operation ImportApi, verdict mismatch:
  divergence_key: service=apigatewayv2|ImportApi|status=200->200|keys=$.apiEndpoint
  status: expected 200, got 200
  $.apiEndpoint: host_not_own (expected 'https://90vho59q5k.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:36585'); affected_count 1.

Cause 7 of 7 - same fixture, step task#2, operation ReimportApi, verdict mismatch:
  divergence_key: service=apigatewayv2|ReimportApi|status=200->200|keys=$.apiEndpoint
  status: expected 200, got 200
  $.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:36585'); affected_count 1.

No HTTP status, error code or error message diverges in any of the seven causes: every response carries the expected success status and only the apiEndpoint member diverges. The generated api ids, the listener ports and the DNS suffix are per-run values; the asserted requirements are (a) the published host carries the (<api-id>, 'execute-api', <region>) leading labels AND is an address the emulator serves, and (b) a WEBSOCKET-protocol API's endpoint uses the wss scheme.

## Steps to Reproduce
Cause 1 of 7 (CreateApi):
1. Replay fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags and stop at step task#1, or by hand: POST /v2/apis {"name":"<any>","protocolType":"HTTP"}.
2. Read the 201 response's `apiEndpoint` and try to connect to urlsplit(apiEndpoint).netloc.
   Expected: leading labels (<apiId>, 'execute-api', <region>) on a suffix/port the emulator's execute-api plane answers on.
   Actual: '<apiId>.execute-api.<region>.localhost:<port>' - announced but not served.

Cause 2 of 7 (GetApi): continue the same fixture to step task#2, or GET /v2/apis/{apiId}; apply the same host check.

Cause 3 of 7 (UpdateApi): replay fixture apigatewayv2-api-stage-bogus-child-not-found to step task#2, or PATCH /v2/apis/{apiId} with {"description":"<any>"}; apply the same host check.

Cause 4 of 7 (GetApis page): replay fixture apigatewayv2-get-apis-membership-and-page-round-trip to step task#4, or create one HTTP API and one WEBSOCKET API then GET /v2/apis; apply the host check to every `items[*].apiEndpoint` and check the WEBSOCKET element's scheme is wss.

Cause 5 of 7 (GetApis membership): continue that fixture to step task#5, or GET /v2/apis and look the API up by `name`; apply the same two checks.

Cause 6 of 7 (ImportApi): replay fixture apigatewayv2-import-reimport-status-codes to step task#1, or PUT /v2/apis with an OpenAPI document body; apply the host check.

Cause 7 of 7 (ReimportApi): continue that fixture to step task#2, or PUT /v2/apis/{apiId} with the exported document; apply the host check.
