Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace apigateway-rest-api-compression-size-unset-by-valueless-replace-binary-types-tags-and-resource-path
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.cacheKeyParameters
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.cacheNamespace
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.connectionType
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.httpMethod
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.passthroughBehavior
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.requestParameters
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.requestTemplates
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.responseTransferMode
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.timeoutInMillis
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.type
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|$.uri
Issue-Key: apigateway|UpdateIntegration|2xx->4xx|status
Issue-Key: apigateway|PutIntegration|2xx->2xx|$.requestTemplates.application/json
Issue-Key: apigateway|GetIntegration|2xx->2xx|$.requestTemplates.application/json
Issue-Key: apigateway|UpdateIntegration|2xx->2xx|$.requestTemplates.application/json
Issue-Key: apigateway|GetMethod|2xx->2xx|$.methodIntegration.requestTemplates.application/json
Issue-Key: apigateway|GetIntegration|2xx->2xx|$.contentHandling
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.apiKeySource
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.apiStatus
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.binaryMediaTypes
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.createdDate
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.disableExecuteApiEndpoint
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.endpointConfiguration
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.id
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.name
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.rootResourceId
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.securityPolicy
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|$.tags
Issue-Key: apigateway|UpdateRestApi|2xx->4xx|status
Issue-Key: apigateway|GetRestApi|2xx->2xx|$.minimumCompressionSize
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# apigateway: valueless PATCH replace is refused and never clears the field, and empty mapping templates report an empty string instead of null

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateIntegration

## Description
8 net-new root causes in the apigateway control plane's PATCH (patchOperations) handling and in the projection of a cleared value. None of the eight clusters carries a stub_path or an owned_by. Two faults run through them: (a) a `replace` patch operation submitted with NO `value` — AWS's documented way to UNSET an optional field — is refused with 400 or silently ignored, and (b) a mapping-template entry whose value is empty is reported as the empty string where AWS reports JSON null.

Common AWS statement: Smithy services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json models UpdateIntegration as PATCH /restapis/{restApiId}/resources/{resourceId}/methods/{httpMethod}/integration -> 200 Integration, and UpdateRestApi as PATCH /restapis/{restApiId} -> 200 RestApi, both taking patchOperations (ListOfPatchOperation, whose doc points at the Patch Operations reference). PutIntegration -> 201 Integration, GetIntegration/GetMethod -> 200. The model states nothing about valueless replaces or about the representation of an empty template value, so these are recorded behavior only; the recordings are quoted per cause.

Common fix location: the patch-application component shared by the apigateway crate's PATCH handlers (the per-operation patch-path allow-list plus its `replace`/`remove` application), the integration and rest-api state that stores the cleared field, and the Integration/Method/RestApi serializers that project a cleared member.

Cause 1 of 8: UpdateIntegration refuses, with 400, the valueless `replace` patch that AWS accepts with 200.
AWS: recorded behavior only — the recorded PATCH on the integration answers 200 with the full Integration body: type HTTP, httpMethod GET, uri, connectionType INTERNET, passthroughBehavior WHEN_NO_MATCH, responseTransferMode BUFFERED, timeoutInMillis, cacheNamespace, cacheKeyParameters [], requestParameters as set, requestTemplates {"application/json": null, "application/xml": "#set($inputRoot = $input.path('$'))\n{ }"}, and NO contentHandling member (it has been cleared). Note the accepted result: the json template entry survives with a NULL value and contentHandling is gone.
Emulator: the same PATCH is answered 400, so none of those members is reported at all.
Why: a client that unsets an integration field the documented way (replace with no value) gets its request refused instead of applied; the integration keeps its previous configuration and the client's subsequent reads are of stale state.
Fix: UpdateIntegration's patch application (its path allow-list and the replace handler that must treat a missing value as a clear).
Traces: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace#task#9.

Cause 2 of 8: GetIntegration still reports `contentHandling` after it was cleared by a valueless replace.
AWS: recorded behavior only — the recorded 200 after the clear carries NO contentHandling member.
Emulator: 200 with contentHandling = CONVERT_TO_BINARY, still present after 30 polls (`$.contentHandling: poll: still present after 30 attempt(s)`).
Why: the integration's payload-conversion strategy a client believes it removed is still in force; a request that should pass through unmodified is converted.
Fix: the integration state's clear path plus GetIntegration's serializer (an unset enum member must be omitted, not retained).
Traces: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace#task#10.

Cause 3 of 8: PutIntegration reports a requestTemplates entry created with an empty template as "" where AWS reports null.
AWS: Smithy Integration.requestTemplates is MapOfStringToString ("a map of Velocity templates ... content type value is the key"). Recorded behavior only: the recorded 201 carries `requestTemplates["application/json"] = null` — the key is present, the value is JSON null.
Emulator: 201 with `requestTemplates["application/json"] = ""` (`$.requestTemplates.application/json: type_mismatch (expected None, got '')`).
Why: the two types differ, not just the text — a client that tests `template is None` (or a typed SDK's optional string) sees a present, empty template and will treat the content type as MAPPED rather than template-less, which changes passthroughBehavior outcomes.
Fix: the integration state's representation of an empty template value plus PutIntegration's serializer.
Traces: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace#task#4.

Cause 4 of 8: GetIntegration reports the same empty template entry as "" instead of null.
AWS/Emulator/Why: as cause 3, on the 200 read path (`$.requestTemplates.application/json: type_mismatch (expected None, got '')`).
Fix: GetIntegration's serializer for the requestTemplates map.
Traces: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace#task#5.

Cause 5 of 8: UpdateIntegration's own 200 response reports the empty template entry as "" instead of null.
AWS/Emulator/Why: as cause 3, on the PATCH response (`$.requestTemplates.application/json: type_mismatch (expected None, got '')`).
Fix: UpdateIntegration's serializer for the requestTemplates map.
Traces: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace#task#6.

Cause 6 of 8: GetMethod reports the embedded methodIntegration's empty template entry as "" instead of null.
AWS/Emulator/Why: as cause 3, under `$.methodIntegration.requestTemplates.application/json` on the 200 GetMethod read.
Fix: the Method serializer's embedded Integration projection (it must share the integration projection the fix for causes 3-5 lands in).
Traces: apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace#task#8.

Cause 7 of 8: UpdateRestApi refuses, with 400, the valueless `replace` of /minimumCompressionSize that AWS accepts with 200.
AWS: Smithy UpdateRestApi -> PATCH /restapis/{restApiId}, 200 RestApi with members id, name, createdDate, binaryMediaTypes, minimumCompressionSize (NullableInteger), apiKeySource, endpointConfiguration, tags, disableExecuteApiEndpoint, rootResourceId, securityPolicy, apiStatus. Recorded behavior only: the recorded PATCH answers 200 with the whole RestApi (apiKeySource HEADER, apiStatus AVAILABLE, binaryMediaTypes, disableExecuteApiEndpoint false, endpointConfiguration, id, name, rootResourceId, securityPolicy TLS_1_0, tags) and NO minimumCompressionSize member — the field has been unset.
Emulator: the same PATCH is answered 400 and reports none of those members.
Why: compression cannot be turned back off the documented way; the API keeps compressing and the client's 400 is indistinguishable from a malformed request.
Fix: UpdateRestApi's patch application (path allow-list + valueless-replace-as-clear) and the RestApi state's unset of a NullableInteger.
Traces: apigateway-rest-api-compression-size-unset-by-valueless-replace-binary-types-tags-and-resource-path#task#3.

Cause 8 of 8: GetRestApi still reports minimumCompressionSize after the valueless replace.
AWS: recorded behavior only — the recorded 200 read after the unset carries NO minimumCompressionSize member.
Emulator: 200 with minimumCompressionSize = 1, still present after 30 polls (`$.minimumCompressionSize: poll: still present after 30 attempt(s)`).
Why: the client's read-back confirms a setting it just removed, so "compression disabled" is unobservable and unreachable.
Fix: the rest-api state's clear path plus GetRestApi's serializer (an unset NullableInteger must be omitted).
Traces: apigateway-rest-api-compression-size-unset-by-valueless-replace-binary-types-tags-and-resource-path#task#4.

Note on values: the api ids, resource ids, cache namespaces, uris and the particular compression size in these recordings are per-run values and are not part of any requirement. The requirements are: a valueless `replace` clears the addressed field and answers 200, a cleared optional member is OMITTED from subsequent representations, and an empty mapping-template entry is reported with a JSON null value.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 2023 steps, 1938 passed, 85 mismatched. All eight clusters report verdict=mismatch, stub_path="", owned_by=[].

Cause 1 — UpdateIntegration, fixture apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace, step task#9: expected_status 200 / actual_status 400. detail: status: expected 200, got 400; and every member of the expected 200 reported missing — $.cacheKeyParameters (expected []), $.cacheNamespace, $.connectionType ('INTERNET'), $.httpMethod ('GET'), $.passthroughBehavior ('WHEN_NO_MATCH'), $.requestParameters ({'integration.request.header.X-Authorization': "'static'", 'integration.request.header.X-Foo': "'Bar'"}), $.requestTemplates ({'application/json': None, 'application/xml': "#set($inputRoot = $input.path('$'))\n{ }"}), $.responseTransferMode ('BUFFERED'), $.timeoutInMillis (2000), $.type ('HTTP'), $.uri ('https://example.com/').

Cause 2 — GetIntegration, same fixture, step task#10: expected_status 200 / actual_status 200. detail: $.requestTemplates.application/json: type_mismatch (expected None, got ''); $.contentHandling: unexpected_present (expected None, got 'CONVERT_TO_BINARY'); $.contentHandling: poll: still present after 30 attempt(s).

Cause 3 — PutIntegration, same fixture, step task#4: expected 201 / actual 201. detail: $.requestTemplates.application/json: type_mismatch (expected None, got '').

Cause 4 — GetIntegration, same fixture, step task#5 (cluster also covers task#7): expected 200 / actual 200. detail: $.requestTemplates.application/json: type_mismatch (expected None, got '').

Cause 5 — UpdateIntegration, same fixture, step task#6: expected 200 / actual 200. detail: $.requestTemplates.application/json: type_mismatch (expected None, got '').

Cause 6 — GetMethod, same fixture, step task#8: expected 200 / actual 200. detail: $.methodIntegration.requestTemplates.application/json: type_mismatch (expected None, got '').

Cause 7 — UpdateRestApi, fixture apigateway-rest-api-compression-size-unset-by-valueless-replace-binary-types-tags-and-resource-path, step task#3: expected_status 200 / actual_status 400. detail: status: expected 200, got 400; $.apiKeySource ('HEADER'), $.apiStatus ('AVAILABLE'), $.binaryMediaTypes (['application/octet-stream']), $.createdDate, $.disableExecuteApiEndpoint (False), $.endpointConfiguration ({'ipAddressType': 'ipv4', 'types': ['EDGE']}), $.id, $.name, $.rootResourceId, $.securityPolicy ('TLS_1_0'), $.tags ({'key1': ...}) all reported missing.

Cause 8 — GetRestApi, same fixture, step task#4: expected 200 / actual 200. detail: $.minimumCompressionSize: unexpected_present (expected None, got 1); $.minimumCompressionSize: poll: still present after 30 attempt(s).

Model evidence: smithy_query query=operation (UpdateIntegration, UpdateRestApi) on services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json — UpdateIntegration PATCH .../integration code 200, input patchOperations: ListOfPatchOperation, output Integration; UpdateRestApi PATCH /restapis/{restApiId} code 200, output RestApi with minimumCompressionSize: NullableInteger. smithy_query query=shape Integration — requestTemplates: MapOfStringToString, contentHandling: ContentHandlingStrategy (optional, no default).

## Steps to Reproduce
1. Point a boto3 `apigateway` client at the emulator and create a REST API with a resource and a GET method.
2. Causes 3-6: `put_integration(..., type='HTTP', integrationHttpMethod='GET', uri='https://example.com/', requestTemplates={'application/json': ''})`. Observe the 201 body reports `requestTemplates['application/json'] == ''`; AWS's recorded 201 reports it as `None`. Repeat the read with `get_integration(...)`, `update_integration(...)` and `get_method(...)` (the embedded `methodIntegration`) — all three report `''`.
3. Cause 1: call `update_integration(..., patchOperations=[{'op': 'replace', 'path': '/contentHandling'}])` — a replace with NO `value`, AWS's documented unset. Observe HTTP 400; AWS answers 200 with the full Integration and no `contentHandling` member.
4. Cause 2: read `get_integration(...)` afterwards — `contentHandling` is still `CONVERT_TO_BINARY` (it stays through 30 polls); AWS omits the member.
5. Cause 7: `update_rest_api(restApiId=api_id, patchOperations=[{'op': 'replace', 'path': '/minimumCompressionSize'}])` (no value) — observe HTTP 400; AWS answers 200 with the full RestApi and no `minimumCompressionSize`.
6. Cause 8: `get_rest_api(restApiId=api_id)` afterwards still reports `minimumCompressionSize` (1), through 30 polls.
7. Full scenarios: replay fixture apigateway-http-integration-empty-json-template-content-handling-cleared-by-empty-replace steps task#4, task#5, task#6, task#8, task#9, task#10 and fixture apigateway-rest-api-compression-size-unset-by-valueless-replace-binary-types-tags-and-resource-path steps task#3, task#4.
