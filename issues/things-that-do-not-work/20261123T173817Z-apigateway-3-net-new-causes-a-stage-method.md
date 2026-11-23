Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.apigw-stage-method-settings-throttle-returns-default-too-many-requests apigateway.delete-domain-name-with-mapping-rejected apigateway.e2e-custom-domain-base-path-routing
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.apigw-stage-method-settings-throttle-returns-default-too-many-requests|run#25|AssertionError|assert served_during_burst, (
Issue-Key: apigateway.delete-domain-name-with-mapping-rejected|_error#18|AssertionError|raise AssertionError("real AWS refuses this call, but it SUCCEEDED")
Issue-Key: apigateway.e2e-custom-domain-base-path-routing|run#38|KeyError|remaining = apigw.get_base_path_mappings(domainName=domain_name)["items"]
Issue-Commit: ec42f8aca5996e01f32b7888d9884c173467aef1

# apigateway: 3 net-new causes — a stage method-settings throttle sheds EVERY request in a burst, DeleteRestApi is accepted while a base path mapping still routes to the API, and an empty GetBasePathMappings page omits the items member

> _Note: this issue was identified against the code base at commit `ec42f8aca5996e01f32b7888d9884c173467aef1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteRestApi

## Description
3 net-new root causes, all in the apigateway (REST API Gateway) emulator, grounded in 3 failing behavioral checks that the replay report marks neither ALREADY FILED nor OWNED by an open issue. Cause 1 is data-plane throttling, causes 2 and 3 are the custom-domain/base-path-mapping control plane.

Cause 1 of 3: a stage's wildcard method-settings throttle sheds EVERY request of a concurrent burst — the configured burst allowance never admits a request.
Failing check: apigateway.apigw-stage-method-settings-throttle-returns-default-too-many-requests
AWS: Smithy (services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json) models the stage method setting shape MethodSetting with throttlingRateLimit (Double, "Specifies the throttling rate limit") and throttlingBurstLimit (Integer, "Specifies the throttling burst limit"), settable per method or for the wildcard `/*/*` key via UpdateStage patchOperations on /*/*/throttling/rateLimit and /*/*/throttling/burstLimit; TooManyRequestsException is modeled at HTTP 429. The model does not state the metering algorithm, so for the data plane this is recorded behavior only: API Gateway meters a deployed stage with a token bucket, so a burst larger than the limit is PARTIALLY shed — at least the burst allowance (here burstLimit=1) is served 200 by the integration while the excess is refused 429 with the default THROTTLED envelope, body `{"message":"Too Many Requests"}` (fixed text, no variable part) and header `x-amzn-ErrorType: TooManyRequestsException`; once the bucket refills, the stage serves 200 again.
Emulator: with rateLimit=1.0 / burstLimit=1 on `/*/*`, a 40-way concurrent GET against the deployed stage returns 429 for ALL 40 requests — not one reaches the MOCK integration. The refusal envelope itself is right (the body and x-amzn-ErrorType assertions pass); the admission decision is wrong. The check fails at `assert served_during_burst` with "a throttle that sheds EVERY request in the burst is not throttling, it is broken; got rejection-only results".
Why: a client that configures any finite throttle on a stage loses the stage entirely under concurrency instead of being shaped — every request is refused, including the ones inside the configured rate and burst. Load-shedding, retry and backoff code cannot be exercised, and a correctly throttled client that stays within its rate still sees 100% 429s.
Fix: the execute-api data-plane admission path that applies the stage's resolved method settings before invoking the integration — the limiter must admit up to the burst allowance (and refill at the rate limit) rather than refusing every request once a limit is configured.
Traces: apigateway.apigw-stage-method-settings-throttle-returns-default-too-many-requests.

Cause 2 of 3: DeleteRestApi SUCCEEDS while an active base path mapping of a custom domain still routes to the API; AWS refuses it with 400 BadRequestException and leaves the API in place.
Failing check: apigateway.delete-domain-name-with-mapping-rejected
AWS: Smithy models DeleteRestApi as DELETE /restapis/{restApiId}, success 202, errors BadRequestException (400), ConflictException (409), NotFoundException (404), TooManyRequestsException (429), UnauthorizedException (401). The model does not state the precondition, so it is recorded behavior only: while any base path mapping of a custom domain names the API, the delete is refused with code `BadRequestException` and the message `Deleting RestApi <restApiId> failed. Please remove all base path mappings related to the RestApi in your domains: <domainNames>` — the fixed text is "Deleting RestApi " / " failed. Please remove all base path mappings related to the RestApi in your domains: ", and the variable parts are the restApiId from the request and the domain name(s) from the emulator's state that still hold a mapping to it (comma-separated when more than one). The API must still be readable by GetRestApi afterwards; once DeleteBasePathMapping removes the mapping, the same DeleteRestApi succeeds.
Emulator: the call is accepted and the API is removed, so the check's `_error(...)` helper raises "real AWS refuses this call, but it SUCCEEDED". services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/delete_rest_api.rs lines 167-193 deliberately implement the opposite rule (a comment states "A reference held by a record this API does not own is no precondition of its delete") and only check `store.rest_apis.contains_key` before removing the record.
Why: this is an over-permissive accept of a destructive call. A client relying on AWS's refusal to protect a live custom-domain route gets the API destroyed instead of a 400, leaving the base path mapping dangling at an id that resolves to nothing; teardown/ordering logic written against AWS (delete mappings, then the API) is never exercised, and the emulator silently permits an outage AWS prevents.
Fix: DeleteRestApi's handler/validation in the apigateway crate — resolve the base path mappings of every stored domain name that names this restApiId and refuse with the BadRequestException above before removing anything.
Traces: apigateway.delete-domain-name-with-mapping-rejected.

Cause 3 of 3: GetBasePathMappings omits the `items` collection member entirely for a custom domain whose mappings have all been deleted, so the "nothing dangling" read raises KeyError.
Failing check: apigateway.e2e-custom-domain-base-path-routing
AWS: Smithy models GetBasePathMappings as GET /domainnames/{domainName}/basepathmappings, success 200, output BasePathMappings with `items` (ListOfBasePathMapping, jsonName "item") and `position`, paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}. The model states nothing about member presence, so this is recorded behavior only: the behavioral recording this check encodes reads `response["items"]` directly after both mappings are deleted and expects `[]`, i.e. a 200 that carries the collection member present-and-empty (wire key `item`).
Emulator: the 200 omits the member, so botocore's parsed response has no `items` key and the read raises `KeyError: 'items'`. services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/get_base_path_mappings.rs lines 197-209 assign `items: page.items_reported_only_when_held(items)` with a comment singling this operation out as "the one collection of this service whose empty answer leaves the member out". Everything the check asserts before this point passes (both mappings are created and resolve to the right restApiId/stage, the deleted root mapping and the deleted "v2" mapping both answer NotFoundException "Invalid base path mapping identifier specified"), so the scenario then aborts before its domain-delete leg is ever reached.
Why: the idiomatic delete-confirmation read (`response["items"] == []`) is unwritable against the emulator for exactly the domain that owns nothing, and a paginator's `items` result key yields no page; a client written against AWS crashes with KeyError on the empty case.
Conflict a fixer must reconcile rather than ping-pong on: FIXED issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md listed this same operation and this same check (its Cause 6 of 9) and claimed the member must always be present — its fix left GetBasePathMappings still omitting it, which is why this check still fails. Open WIRE issue 20261115T090346Z-apigateway-listing-envelope-diverges-from-the-recording-an.md asserts the OPPOSITE for this operation (a recorded real-AWS empty 200 whose body is `{}`), and the current source comment follows that issue. Neither issue covers this check today (the report marks it neither filed nor owned). The two corpora must be reconciled for this one operation: whichever wire body is correct, this behavioral check's read of an emptied domain's mappings has to stop raising KeyError.
Fix: GetBasePathMappings' response path plus the shared `crate::state::Page` reporting helpers (`items_reported_only_when_held` / `reported_items`) in the apigateway crate's state.
Traces: apigateway.e2e-custom-domain-base-path-routing.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout: 989/1077 checks passed; of the 88 failures, these 3 are the only ones under the `apigateway` heading that the report marks neither "ALREADY FILED" nor "OWNED by" an open issue.

Cause 1 — apigateway.apigw-stage-method-settings-throttle-returns-default-too-many-requests [mismatch]
  Setup: REGIONAL REST API, root GET with a MOCK integration returning `{"served":true}`, deployed to stage `test`, then UpdateStage patchOperations replace /*/*/throttling/rateLimit = "1.0" and /*/*/throttling/burstLimit = "1". A single unburst GET is served 200 `{"served": true}` (precondition passes).
  Expected (AWS): in a 40-way concurrent burst, some requests are shed 429 with body `{"message":"Too Many Requests"}` and header `x-amzn-ErrorType: TooManyRequestsException`, AND at least the burst allowance is served 200 by the integration; after the bucket refills a further GET is 200 again.
  Actual (emulator): every request of the burst is 429. The 429 body and x-amzn-ErrorType assertions pass; the next assertion fails:
    File "check_apigateway_apigw_stage_method_settings_throttle_returns_default_too_many_requests.py", line 163, in run
      assert served_during_burst, (
    AssertionError: a throttle that sheds EVERY request in the burst is not throttling, it is broken; got rejection-only results for <stage invoke URL>
  (The host:port and api id in that message are per-run values and are not part of the requirement.)

Cause 2 — apigateway.delete-domain-name-with-mapping-rejected [mismatch]
  Setup: a REGIONAL custom domain with an imported ACM certificate reaches AVAILABLE/FAILED, a REST API with a MOCK GET deployed to stage `live`, and CreateBasePathMapping(basePath="v1", restApiId=<api>, stage="live"); the mapping is confirmed visible via both GetBasePathMapping and GetBasePathMappings before the delete.
  Expected (AWS): DeleteRestApi is refused — Code `BadRequestException` (HTTP 400), Message `Deleting RestApi <restApiId> failed. Please remove all base path mappings related to the RestApi in your domains: <domainName>` — and GetRestApi still returns the API. After DeleteBasePathMapping, DeleteRestApi succeeds and GetRestApi then answers NotFoundException `Invalid API identifier specified <accountId>:<restApiId>`.
  Actual (emulator): DeleteRestApi returns success (202) and the API is removed:
    File "check_apigateway_delete_domain_name_with_mapping_rejected.py", line 73, in run
      code, message = _error(lambda: apigw.delete_rest_api(restApiId=rest_api_id))
    File "...", line 143, in _error
      raise AssertionError("real AWS refuses this call, but it SUCCEEDED")
    AssertionError: real AWS refuses this call, but it SUCCEEDED
  Emulator source: src/ops/delete_rest_api.rs lines 167-193 (comment: the mapping "is neither refused on nor removed here").

Cause 3 — apigateway.e2e-custom-domain-base-path-routing [error]
  Setup: a REGIONAL custom domain with two base path mappings (""→prod, "v2"→beta) on one REST API; both resolve correctly, both are then deleted and both report NotFoundException "Invalid base path mapping identifier specified" on read and on UpdateBasePathMapping.
  Expected (AWS): `apigw.get_base_path_mappings(domainName=<domain>)["items"] == []`.
  Actual (emulator): the 200 omits the member:
    File "check_apigateway_e2e_custom_domain_base_path_routing.py", line 99, in run
      remaining = apigw.get_base_path_mappings(domainName=domain_name)["items"]
    KeyError: 'items'
  Emulator source: src/ops/get_base_path_mappings.rs lines 197-209, `items: page.items_reported_only_when_held(items)`; helper in src/state.rs.
  Prior art: FIXED issue 20261114T023619Z (claimed this operation/check; fix left it failing) and OPEN wire issue 20261115T090346Z (asserts the recorded empty body is `{}`) directly contradict each other for this operation.

Note on values: domain names, rest api ids, stage invoke hosts/ports, certificate ARNs and account ids in these runs are per-run values and are not part of any requirement here. The requirements are the admission behaviour of a configured throttle, the refusal code and message template of DeleteRestApi under an active base path mapping, and the structural presence of the empty listing's collection member.

## Steps to Reproduce
Cause 1:
1. Point a boto3 `apigateway` client at the emulator; CreateRestApi (REGIONAL), PutMethod GET authorizationType=NONE on the root resource, PutIntegration type=MOCK with requestTemplates {"application/json": "{\"statusCode\": 200}"}, PutMethodResponse 200, PutIntegrationResponse 200 with responseTemplates {"application/json": "{\"served\":true}"}.
2. CreateDeployment, CreateStage(stageName="test"), then UpdateStage with patchOperations [replace /*/*/throttling/rateLimit = "1.0", replace /*/*/throttling/burstLimit = "1"].
3. GET the stage URL once: it is served 200 `{"served": true}`.
4. Issue 40 concurrent GETs to the same URL. Observed: all 40 answer 429. Expected: the shed requests answer 429 `{"message":"Too Many Requests"}` with x-amzn-ErrorType: TooManyRequestsException while at least the burst allowance is served 200 `{"served": true}`, and a GET after the bucket refills is 200 again.
5. Full scenario: replay check apigateway.apigw-stage-method-settings-throttle-returns-default-too-many-requests.

Cause 2:
1. ImportCertificate into ACM for <domain>, CreateDomainName(domainName=<domain>, regionalCertificateArn=<arn>, endpointConfiguration={"types":["REGIONAL"]}) and poll GetDomainName until domainNameStatus is AVAILABLE or FAILED.
2. CreateRestApi, PutMethod/PutIntegration (MOCK) on the root GET, CreateDeployment(stageName="live").
3. CreateBasePathMapping(domainName=<domain>, basePath="v1", restApiId=<api>, stage="live") and confirm it via GetBasePathMapping and GetBasePathMappings.
4. Call DeleteRestApi(restApiId=<api>). Observed: it succeeds and the API is gone. Expected: 400 BadRequestException `Deleting RestApi <restApiId> failed. Please remove all base path mappings related to the RestApi in your domains: <domain>`, with GetRestApi still returning the API.
5. DeleteBasePathMapping, then DeleteRestApi — this must now succeed, and GetRestApi must answer NotFoundException `Invalid API identifier specified <accountId>:<restApiId>`.
6. Full scenario: replay check apigateway.delete-domain-name-with-mapping-rejected.

Cause 3:
1. Create a REGIONAL custom domain as above, a REST API with a MOCK root GET, a deployment and two stages (prod, beta).
2. CreateBasePathMapping(basePath="", stage="prod") and CreateBasePathMapping(basePath="v2", stage="beta"); both resolve correctly through GetBasePathMappings.
3. DeleteBasePathMapping(basePath="(none)") and DeleteBasePathMapping(basePath="v2").
4. Read `apigw.get_base_path_mappings(domainName=<domain>)["items"]`. Observed: HTTP 200 whose body carries no `item` key, so the read raises KeyError: 'items'. Expected: `[]`.
5. Full scenario: replay check apigateway.e2e-custom-domain-base-path-routing.
