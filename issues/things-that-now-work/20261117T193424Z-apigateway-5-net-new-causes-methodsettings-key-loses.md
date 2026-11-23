Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.deployment-throttling-burst-limit-window apigateway.import-documentation-parts-invalid-mode-rejected apigateway.apigw-openapi-import-request-validator-extension-enforced-at-the-stage apigateway.resolved-resource-missing-method-returns-missing-authentication-token apigateway.test-invoke-method-stage-variables-resolve-without-a-deployed-stage
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.deployment-throttling-burst-limit-window|run#14|AssertionError|assert "*/*" in settings and "~1/GET" in settings, (
Issue-Key: apigateway.import-documentation-parts-invalid-mode-rejected|run#4|KeyError|before_ids = {p["id"] for p in apigw.get_documentation_parts(restApiId=api_id)["items"]}
Issue-Key: apigateway.apigw-openapi-import-request-validator-extension-enforced-at-the-stage|run#6|AssertionError|assert set(declared) == {_STRICT_VALIDATOR, _LOOSE_VALIDATOR}, (
Issue-Key: apigateway.resolved-resource-missing-method-returns-missing-authentication-token|run#31|AssertionError|assert status == 200, (
Issue-Key: apigateway.test-invoke-method-stage-variables-resolve-without-a-deployed-stage|run#18|AssertionError|assert alpha["status"] == 200, f"expected status 200 for target=alpha, got {alpha['status']} (log={alpha.get('log', '')[:300]!r})"
Issue-Commit: 5782d860642bee99336bbb1ce42eb572c637eba4

# apigateway: 5 net-new causes — methodSettings key loses its ~1 escape, an empty GetDocumentationParts omits items, the OpenAPI request-validator extension is ignored, method fall-through to a parameterised sibling is refused, and TestInvokeMethod does not resolve stageVariables

> _Note: this issue was identified against the code base at commit `5782d860642bee99336bbb1ce42eb572c637eba4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStage

## Description
5 net-new root causes in the apigateway service: two representation defects, one import defect and two data-plane/test-invoke defects. Each has its own failing check and must be closed on its own.

Smithy model referenced throughout: services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json.

Cause 1 of 5: GetStage reports a per-method methodSettings key with its JSON-Pointer escape stripped, so the wildcard default and the method override cannot coexist under the keys the client wrote.
AWS: Smithy models Stage$methodSettings as a map whose keys are method paths "{resource_path}/{http_method}" for an individual override or "*/*" for the stage-wide default. Stage settings are written with JSON-Patch paths (RFC 6901), where a '/' inside a key segment is escaped '~1'; an override on the root resource's GET is therefore written '/~1/GET/...' and read back under the key '~1/GET'. Recorded behavior for the exact spelling: GetStage reports both keys side by side — '*/*' (the stage-wide default) and '~1/GET' (the override) — neither merged nor rewritten, stable across repeated GetStage calls.
Emulator: GetStage reports keys ['*/*', '/GET'] — the override key has lost its '~1' escape, so a client reading methodSettings['~1/GET'] misses the entry it just created.
Why: the key is a value the CLIENT supplied; with the escape stripped every lookup and every follow-up patch keyed on the escaped form misses, and tooling concludes the per-method throttle/metrics override it just set does not exist.
Fix: the methodSettings map key in the stage state and the GetStage/UpdateStage serialization (src/ops/get_stage.rs, src/ops/update_stage.rs and the Stage record in src/state.rs) — store and emit the escaped key form verbatim.
Related, NOT covering: open issue 20261117T121318Z-apigateway-cloudwatch-apigateway-getstage-reports-methodsettings-keyed-by-the-unescaped.md reports this same unescaping for a different check under the apigateway__cloudwatch heading ('~1ok/GET' -> 'ok/GET'); it does not carry this check, which additionally pins the coexistence of '*/*' with the escaped override key for the ROOT resource ('~1/GET').
Traces: apigateway.deployment-throttling-burst-limit-window.

Cause 2 of 5: GetDocumentationParts omits the items member for an API with no documentation parts.
AWS: Smithy GetDocumentationParts (GET /restapis/{restApiId}/documentation/parts, success 200) models output `items` (ListOfDocumentationPart, wire key "item") + `position`. The model does not state member presence, so for the empty case this is recorded behavior only: a 200 listing always carries its collection member, surfaced by SDKs as `items`, empty list when the API holds nothing.
Emulator: the 200 body omits `item` entirely, so the parsed response has no `items` key and the idiomatic read raises KeyError: 'items' — here on the very first call of the scenario, which aborts before ImportDocumentationParts' invalid-mode refusal (expected ValidationException "1 validation error detected: Value '<mode>' at 'mode' failed to satisfy constraint: Member must satisfy enum value set: [overwrite, merge]" — fixed text with the rejected mode value from the request) is ever asserted.
Why: every "is it listed / is it gone?" baseline on a fresh API is unwritable, and the enum refusal under test is never reached.
Fix: the GetDocumentationParts response path (src/ops/get_documentation_parts.rs) and the shared Page::reported_items helper in src/state.rs — report a present, empty collection member.
Related, NOT covering: open issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md names this operation as its Cause 4 of 9 but does not carry this check; a fix to the shared helper may close both, a fix scoped to the three checks that issue lists will not.
Traces: apigateway.import-documentation-parts-invalid-mode-rejected.

Cause 3 of 5: ImportRestApi ignores the x-amazon-apigateway-request-validators extension, so the imported API holds no request validators and nothing is validated at the stage.
AWS: recorded behavior (the extension is an OpenAPI vendor extension, not a Smithy member): an ImportRestApi with failOnWarnings=true and an x-amazon-apigateway-request-validators map creates ONE RequestValidator per entry of that map, carrying the declared validateRequestBody/validateRequestParameters flags; the API-level x-amazon-apigateway-request-validator selects the default, a method-level one overrides it, and GetMethod reports each method's requestValidatorId accordingly. At the deployed stage a body missing a required property is then refused 400 with the BAD_REQUEST_BODY gateway response body {"message":"Invalid request body"} before the integration runs, while the twin method whose validator checks parameters only serves the identical body 200.
Emulator: the import succeeds but the API holds NO validators at all — GetRequestValidators lists [] where the two declared validator names were expected — so no method carries a requestValidatorId and the stage validates nothing.
Why: an OpenAPI/Swagger definition that AWS imports as a validating API imports here as a non-validating one; requests AWS refuses 400 reach the integration, which is a silent correctness and security difference for every imported definition that relies on the extension.
Fix: the OpenAPI import path for the request-validator extensions (the ImportRestApi/PutRestApi handler, src/ops/import_rest_api.rs, and the validator/method records it creates in src/state.rs).
Traces: apigateway.apigw-openapi-import-request-validator-extension-enforced-at-the-stage.

Cause 4 of 5: at a deployed stage, a request whose literal resource does not declare the method is refused instead of falling through to the parameterised sibling that does declare it.
AWS: recorded behavior of execute-api method resolution: a literal child resource wins for every method it declares; for a method it does NOT declare, the request resolves against the parameterised sibling that declares it, binding the literal path segment as the path parameter's value (PUT /orders/pending is served by {orderId}'s PUT with orderId="pending"). Only when NEITHER resource declares the method does the stage answer HTTP 403 with the exact body {"message":"Missing Authentication Token"} — that status, and that fixed message, scoped to the one undeclared method while the sibling methods keep serving 200.
Emulator: PUT /orders/pending answers 403 {"message":"Missing Authentication Token"} even though the parameterised sibling declares PUT, i.e. the fall-through never happens and the Missing-Authentication-Token refusal is over-applied.
Why: a client's working route stops being served: any API that mixes a literal resource with a parameterised sibling loses every method the literal one does not spell out, and a 403 Missing Authentication Token (which callers read as a signing/credentials problem) is returned for a route that is correctly configured.
Fix: method resolution in the execute-api data plane — the literal-vs-greedy/parameterised candidate walk must retry the parameterised sibling when the literal resource has no such method, and only then fall back to the MISSING_AUTHENTICATION_TOKEN gateway response.
Traces: apigateway.resolved-resource-missing-method-returns-missing-authentication-token.

Cause 5 of 5: TestInvokeMethod does not resolve ${stageVariables.*} in an HTTP integration URI from the stageVariables it is called with.
AWS: Smithy TestInvokeMethod (POST /restapis/{restApiId}/resources/{resourceId}/methods/{httpMethod}, success 200) models input `stageVariables` (MapOfStringToString) and output `status`, `body`, `headers`, `log`, `latency`. Recorded behavior: the stageVariables argument supplies the values for ${stageVariables.*} references in the method's integration URI even though the API has NEVER been deployed (GetDeployments stays empty); calling it with {"target":"alpha"} answers status 200 and the alpha backend's body, and with {"target":"beta"} the beta backend's body — the same method, two different resolved URIs.
Emulator: answers status 500 for both, with the execution log truncated right after "Method request body before transformations:", so the integration URI's stage-variable reference is never resolved and the backend is never reached.
Why: TestInvokeMethod is the only way to exercise a method before deploying it; a client gets a 500 for a configuration AWS serves 200, and stage-variable-selected backends cannot be tested at all.
Fix: the TestInvokeMethod handler's integration-URI resolution (src/ops/test_invoke_method.rs) — substitute the request's stageVariables into the integration URI (and into the execution log) before invoking the integration.
Traces: apigateway.test-invoke-method-stage-variables-resolve-without-a-deployed-stage.

## Evidence
Cause 1 — expected: GetStage methodSettings holds both '*/*' and '~1/GET'. Actual: "AssertionError: expected both the wildcard default key '*/*' and the method-specific override key '~1/GET' to be retained side by side, got keys ['*/*', '/GET']" — the key's fixed form is the escaped '{resource_path}/{http_method}'; the resource ('/') and method (GET) come from the request.

Cause 2 — expected: HTTP 200 whose body carries the wire key "item", so `get_documentation_parts(restApiId=...)["items"] == []` on a fresh API. Actual: "KeyError: 'items'" at `before_ids = {p["id"] for p in apigw.get_documentation_parts(restApiId=api_id)["items"]}". The refusal the check exists to assert (ValidationException, "1 validation error detected: Value 'bogus-mode' at 'mode' failed to satisfy constraint: Member must satisfy enum value set: [overwrite, merge]") is never reached.

Cause 3 — expected: the import creates one validator per entry of x-amazon-apigateway-request-validators (two here, one body-only and one parameters-only), POST on the API-default path inherits the API-level validator, the override path names the other, and the stage answers 400 {"message":"Invalid request body"} for a body missing a required property. Actual: "AssertionError: the import must create one validator per entry of the extension map; the API holds [], expected ['parcelbody', 'parcelparams']" — the validator names are the definition's own; the requirement is one validator per declared entry, not these names.

Cause 4 — expected: 200 from the parameterised sibling's PUT with the literal segment bound as the path parameter. Actual: "AssertionError: expected 200 from the parameterised sibling (the literal resource has no PUT), got 403: '{\"message\":\"Missing Authentication Token\"}'". The 403 + that exact body is AWS's answer only when NO resource on the resolved path declares the method.

Cause 5 — expected: status 200 and body '{"route": "alpha"}' (then '{"route": "beta"}' for the second call), with no deployment on the subject API. Actual: "AssertionError: expected status 200 for target=alpha, got 500 (log='Execution log for request test-invoke-request\n... : HTTP Method: GET, Resource Path: /route\n... : Method request headers: []\n... : Method request body before transformations:')" — the log's timestamps and request id are per-run values; the divergence is the 500 and the unresolved URI.

## Steps to Reproduce
Point a boto3 `apigateway` client at the emulator.
1. Cause 1: CreateRestApi; put a GET method + MOCK integration + method/integration response on the root resource; CreateDeployment; CreateStage; set stage throttling with UpdateStage patch paths '/*/*/throttling/...' and '/~1/GET/throttling/...'; then GetStage and inspect methodSettings keys — AWS reports ['*/*', '~1/GET'], the emulator reports ['*/*', '/GET'].
2. Cause 2: CreateRestApi, then `get_documentation_parts(restApiId=api_id)["items"]` — expect []; the emulator raises KeyError: 'items'. Then `import_documentation_parts(restApiId=api_id, mode="bogus-mode", body=<swagger with x-amazon-apigateway-documentation>)` and compare the ValidationException message.
3. Cause 3: `import_rest_api(failOnWarnings=True, body=<OpenAPI with x-amazon-apigateway-request-validators holding two entries, an API-level x-amazon-apigateway-request-validator and a method-level override>)`; then `get_request_validators(restApiId=<new id>)` — AWS lists both declared validators; the emulator lists none. Deploy a stage and POST a body missing a required property: AWS answers 400 {"message":"Invalid request body"}, the emulator serves the integration.
4. Cause 4: create /orders, /orders/pending (GET, POST) and /orders/{orderId} (GET, PUT), all MOCK; deploy a stage; then `PUT <stage>/orders/pending` — AWS serves 200 from {orderId}'s PUT with orderId="pending"; the emulator answers 403 {"message":"Missing Authentication Token"}. `DELETE <stage>/orders/pending` (declared by neither) must be the only 403.
5. Cause 5: deploy a backend REST API with /alpha and /beta MOCK routes; on a SECOND, never-deployed API put a GET method whose HTTP integration uri is "<backend stage url>/${stageVariables.target}"; call `test_invoke_method(restApiId=..., resourceId=..., httpMethod="GET", stageVariables={"target":"alpha"})` — expect status 200 and the alpha body; the emulator answers 500.
Full scenarios: replay the five check ids in trace_ids.
