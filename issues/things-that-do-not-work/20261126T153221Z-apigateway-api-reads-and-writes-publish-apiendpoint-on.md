Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-cors-configuration-lifecycle
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-cors-configuration-lifecycle apigatewayv2-cors-configuration-validation-errors apigatewayv2-http-api-cors-accesslog-cache-cleanup apigatewayv2-http-api-lifecycle apigatewayv2-models-and-cors apigatewayv2-export-import-reimport-roundtrip apigatewayv2-import-api-basepath-split apigatewayv2-import-api-failonwarnings-rejected apigatewayv2-import-export-reimport-api
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|GetApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigateway|ImportApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigateway|UpdateApi|2xx->2xx|$.apiEndpoint
Issue-Key: apigateway|GetApis|2xx->2xx|$.items[*].apiEndpoint
Issue-Commit: 4c0072743c4b802ee07d254916613d27e39cf370

# apigateway Api reads and writes publish apiEndpoint on an unserved execute-api .localhost host

> _Note: this issue was identified against the code base at commit `4c0072743c4b802ee07d254916613d27e39cf370`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetApi

## Description
FOUR net-new causes, one shared fault. Every operation that serializes the apigatewayv2 `Api` view under the `apigateway` service token derives the response's `apiEndpoint` from the emulator's own data listener and appends a `.localhost:<port>` suffix to the three documented leading labels. The resulting URI has the right leading-label structure but names a host that is NOT an address the emulator is reached on, so the replay classifies it `host_not_own`: the emulator announces an endpoint it does not answer on. The four causes below are four separate response serializers building the same field wrongly; each must be closed.

Cause 1 of 4 - GetApi (fixture apigatewayv2-cors-configuration-lifecycle, step task#2).
AWS: Smithy apigatewayv2 GetApi -> GetApiResponse.ApiEndpoint (jsonName `apiEndpoint`, target __string), HTTP GET /v2/apis/{ApiId}, success code 200. Member doc on Api.ApiEndpoint: "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." The requirement is the URI form AND its reachability: an absolute `https://` URI whose hostname's leading dot-separated labels are, in order, the API's own id (the same value in `apiId`), the literal label `execute-api`, and the region, on a DNS suffix the execute-api data plane actually serves. The recorded response status is 200 and no error is involved; only this member diverged. The generated api id, the suffix and the port are per-run environment values, not part of the requirement.
Emulator: GetApi answers 200 but publishes `https://<api-id>.execute-api.<region>.localhost:<port>` (replay detail: `$.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:42271')`). The `.localhost` suffix plus listener port is not a host the emulator serves the execute-api plane on, so the announced address is unusable.
Why: a client that follows the documented contract uses `apiEndpoint` as the base URL of the deployed API and derives the SigV4 signing region and service from its host. Against the emulator the host resolves to nothing it answers on, so every data-plane call built from a GetApi read fails to connect even though the control-plane read succeeded.
Fix: the shared Api-endpoint derivation feeding the GetApi response serializer must emit a host the emulator's execute-api data plane is actually reachable on, keeping the (<api-id>, `execute-api`, <region>) leading labels.
Traces: apigatewayv2-cors-configuration-lifecycle#task#2 (cluster of 10: also apigatewayv2-cors-configuration-lifecycle#task#4, apigatewayv2-cors-configuration-validation-errors#task#5 and #task#7, apigatewayv2-http-api-cors-accesslog-cache-cleanup#task#3 and #task#5, apigatewayv2-http-api-lifecycle#task#2 and #task#4, apigatewayv2-models-and-cors#task#2 and #task#4).

Cause 2 of 4 - ImportApi (fixture apigatewayv2-export-import-reimport-roundtrip, step task#4).
AWS: Smithy apigatewayv2 ImportApi -> ImportApiResponse.ApiEndpoint (jsonName `apiEndpoint`), HTTP PUT /v2/apis, modeled success code 201; the replayed step records 200. ImportApi returns the same `Api` view, so `apiEndpoint` carries the identical documented form and must name the newly imported API's own reachable execute-api host.
Emulator: answers 200 but publishes `https://<api-id>.execute-api.<region>.localhost:<port>` (replay detail: `$.apiEndpoint: host_not_own (expected '<shaped-not-asserted>', got 'https://e77k44w5b0.execute-api.us-west-2.localhost:38751')`).
Why: an OpenAPI import is immediately followed by data-plane traffic against the returned endpoint; the client cannot reach the API it just imported.
Fix: same Api-endpoint derivation, as reused by the ImportApi response serializer.
Traces: apigatewayv2-export-import-reimport-roundtrip#task#4 (cluster of 7: also apigatewayv2-import-api-basepath-split#task#1, #task#3, #task#5, #task#8, apigatewayv2-import-api-failonwarnings-rejected#task#2, apigatewayv2-import-export-reimport-api#task#1).

Cause 3 of 4 - UpdateApi (fixture apigatewayv2-http-api-lifecycle, step task#3).
AWS: Smithy apigatewayv2 UpdateApi -> UpdateApiResponse.ApiEndpoint (jsonName `apiEndpoint`), HTTP PATCH /v2/apis/{ApiId}; the replayed step records 201. Updating an API must not change the endpoint's form: it stays the reachable execute-api host of the API named by `{ApiId}`.
Emulator: answers 201 but publishes `https://<api-id>.execute-api.<region>.localhost:<port>` (replay detail: `$.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46489')`).
Why: a client cannot repair the endpoint by re-reading it after an update - the write path reports the same unserved host.
Fix: same derivation, in the UpdateApi response serializer.
Traces: apigatewayv2-http-api-lifecycle#task#3 (cluster of 2: also apigatewayv2-models-and-cors#task#3).

Cause 4 of 4 - GetApis (fixture apigatewayv2-http-api-lifecycle, step task#5).
AWS: Smithy apigatewayv2 GetApis -> GetApisResponse.Items (jsonName `items`, target __listOfApi), HTTP GET /v2/apis, success code 200. Each list element is the same `Api` shape, so every `items[*].apiEndpoint` carries the documented reachable execute-api host form for that element's own api id.
Emulator: answers 200 but every element's `apiEndpoint` is the unserved `.localhost:<port>` host (replay detail: `$.items[0].apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46489')`).
Why: an enumerate-then-call client (console listings, inventory tooling) gets an unreachable base URL for every API in the account at once.
Fix: the list serializer must build each element's endpoint through the same corrected derivation rather than the listener address.
Traces: apigatewayv2-http-api-lifecycle#task#5.

Related but NOT covering: open issue 20261125T063702Z-apigateway-createapi-and-reimportapi-publish-apiendpoint-as-a.md carries the replay keys for this service's CreateApi and ReimportApi apiEndpoint clusters only (and describes the older path-style `/_aws/execute-api/<id>` form); the four clusters above are reported unowned by the replay, so their serializers are still unfixed and unexercised by it. The open behavioural issues 20261126T140544Z-apigatewayv2-the-published-apiendpoint-names-an-unserved-api.md and 20261126T140244Z-apigateway-apigatewayv2-reopens-an-http-api-s-published-apiendpoint-is--r2.md describe the same unserved-host fault from the data-plane side under other service tokens and carry no replay key for these fixture steps.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, 1057 steps (952 passed, 104 mismatched, 1 errored), 34 divergence clusters, score_pct 90.1. All four clusters below have empty `stub_path` and empty `owned_by`.

Cause 1 of 4 - fixture apigatewayv2-cors-configuration-lifecycle, step task#2, service apigateway, operation GetApi, verdict mismatch:
  divergence_key: service=apigateway|GetApi|status=200->200|keys=$.apiEndpoint
  status: expected 200, got 200 (status agrees)
  $.apiEndpoint: host_not_own
    Expected: an execute-api URI on a host the service answers on, leading labels (<apiId>, 'execute-api', <region>) - recorded run 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com'.
    Actual:   'https://mp67t4dbd4.execute-api.us-west-2.localhost:42271' - a '.localhost:<port>' host the emulator is not reached on.
  affected_count: 10.

Cause 2 of 4 - fixture apigatewayv2-export-import-reimport-roundtrip, step task#4, service apigateway, operation ImportApi, verdict mismatch:
  divergence_key: service=apigateway|ImportApi|status=200->200|keys=$.apiEndpoint
  status: expected 200, got 200 (status agrees)
  $.apiEndpoint: host_not_own (expected '<shaped-not-asserted>', got 'https://e77k44w5b0.execute-api.us-west-2.localhost:38751')
  affected_count: 7.

Cause 3 of 4 - fixture apigatewayv2-http-api-lifecycle, step task#3, service apigateway, operation UpdateApi, verdict mismatch:
  divergence_key: service=apigateway|UpdateApi|status=201->201|keys=$.apiEndpoint
  status: expected 201, got 201 (status agrees)
  $.apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46489')
  affected_count: 2.

Cause 4 of 4 - fixture apigatewayv2-http-api-lifecycle, step task#5, service apigateway, operation GetApis, verdict mismatch:
  divergence_key: service=apigateway|GetApis|status=200->200|keys=$.items[0].apiEndpoint
  status: expected 200, got 200 (status agrees)
  $.items[0].apiEndpoint: host_not_own (expected 'https://mp67t4dbd4.execute-api.us-west-2.amazonaws.com', got 'https://mp67t4dbd4.execute-api.us-west-2.localhost:46489')
  affected_count: 1.

No HTTP status, error code or error message diverges in any of the four causes: every response carries the expected success status and only the apiEndpoint member's host diverges. The generated api ids, the listener ports and the DNS suffix are per-run values; the asserted requirement is that the published host carries the (<api-id>, 'execute-api', <region>) leading labels AND is an address the emulator serves.

## Steps to Reproduce
Cause 1 of 4 (GetApi):
1. Replay fixture apigatewayv2-cors-configuration-lifecycle and stop at step task#2, or by hand: POST /v2/apis {"name":"<any>","protocolType":"HTTP"}, then GET /v2/apis/{apiId}.
2. Read the 200 response's `apiEndpoint` and try to connect to urlsplit(apiEndpoint).netloc.
   Expected: host leading labels (<apiId>, 'execute-api', <region>) on a suffix/port the emulator's execute-api plane answers on.
   Actual: '<apiId>.execute-api.<region>.localhost:<port>' - announced but not served.

Cause 2 of 4 (ImportApi):
1. Replay fixture apigatewayv2-export-import-reimport-roundtrip through step task#4, or by hand: PUT /v2/apis with an OpenAPI document body.
2. Apply the same host check to the response's `apiEndpoint`.

Cause 3 of 4 (UpdateApi):
1. Replay fixture apigatewayv2-http-api-lifecycle through step task#3, or by hand: PATCH /v2/apis/{apiId} with {"description":"<any>"}.
2. Apply the same host check to the response's `apiEndpoint`.

Cause 4 of 4 (GetApis):
1. Replay fixture apigatewayv2-http-api-lifecycle through step task#5, or by hand: GET /v2/apis.
2. Apply the same host check to every `items[*].apiEndpoint`.
