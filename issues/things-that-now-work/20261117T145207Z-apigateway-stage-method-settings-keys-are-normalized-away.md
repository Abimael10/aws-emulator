Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-stage-method-settings-unthrottled-mock-method-test-invoke-and-not-found
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-stage-method-settings-unthrottled-mock-method-test-invoke-and-not-found apigateway-stage-method-settings-duplicate-spelling apigateway-stage-method-settings-path-grammar
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.cacheClusterEnabled
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.cacheClusterStatus
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.createdDate
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.deploymentId
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.lastUpdatedDate
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.methodSettings
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.stageName
Issue-Key: apigateway|UpdateStage|2xx->4xx|$.tracingEnabled
Issue-Key: apigateway|UpdateStage|2xx->4xx|status
Issue-Key: apigateway|GetStage|2xx->2xx|$.methodSettings.test/GET
Issue-Key: apigateway|GetStage|2xx->2xx|$.methodSettings.test/GET.throttlingBurstLimit
Issue-Key: apigateway|UpdateStage|2xx->2xx|$.methodSettings.pets/GET
Issue-Key: apigateway|UpdateStage|2xx->2xx|$.methodSettings.~1pets/GET
Issue-Key: apigateway|GetStage|2xx->2xx|$.methodSettings.pets/GET
Issue-Key: apigateway|GetStage|2xx->2xx|$.methodSettings.~1pets/GET
Issue-Related: 20261107T204751Z-apigateway-api-gateway-stage-method-settings-duplicate-resource-path.md
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# apigateway stage method settings: keys are normalized away and unthrottled patches are refused

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStage

## Description
5 net-new root causes, all in the apigateway control plane's Stage method-settings handling (UpdateStage's patch application and the Stage representation's methodSettings map). None of these five clusters carries a stub_path or an owned_by.

Common AWS statement: Smithy services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json models UpdateStage as PATCH /restapis/{restApiId}/stages/{stageName} -> 200 Stage, input patchOperations (ListOfPatchOperation), and GetStage -> 200 Stage; Stage.methodSettings is MapOfMethodSettings (key String -> MethodSetting with metricsEnabled, loggingLevel, dataTraceEnabled, throttlingBurstLimit: Integer, throttlingRateLimit: Double, cachingEnabled, cacheTtlInSeconds, cacheDataEncrypted, requireAuthorizationForCacheControl, unauthorizedCacheControlHeaderStrategy). The model constrains neither the map KEY's spelling nor the throttling values, so both are recorded behavior only and are quoted per cause.

Related, NOT covering: open issue 20261107T204751Z-apigateway-api-gateway-stage-method-settings-duplicate-resource-path.md owns the separate cluster where UpdateStage fails to REFUSE two settings that collide on the same resource path. It does not carry any of the five replay keys below, and fixing the duplicate refusal alone leaves all five failing.

Common fix location: UpdateStage's patch application for the /{resourcePath}/{httpMethod}/... method-setting paths (the path parser that currently canonicalises the key, and its value validation), plus the Stage state and the Stage serializer that project the methodSettings map.

Cause 1 of 5: UpdateStage refuses, with 400, a method-setting patch that sets throttling to the UNTHROTTLED sentinel, where AWS accepts it with 200.
AWS: recorded behavior only — the recorded PATCH answers 200 with the full Stage: stageName, deploymentId, cacheClusterEnabled false, cacheClusterStatus NOT_AVAILABLE, tracingEnabled false, createdDate/lastUpdatedDate, and methodSettings {"test/GET": {metricsEnabled false, dataTraceEnabled false, throttlingBurstLimit -1, throttlingRateLimit -1.0, cachingEnabled false, cacheTtlInSeconds 300, cacheDataEncrypted false, requireAuthorizationForCacheControl true, unauthorizedCacheControlHeaderStrategy SUCCEED_WITH_RESPONSE_HEADER}} — i.e. -1 / -1.0 is a legal, persisted "no throttle" value.
Emulator: the same PATCH is answered 400 and reports none of those members.
Why: a client cannot express "remove the per-method throttle" at all; its request is refused and the stage keeps whatever limits it had.
Fix: UpdateStage's method-setting value validation (the throttling burst/rate bounds must admit the unthrottled sentinel) in the patch application.
Traces: apigateway-stage-method-settings-unthrottled-mock-method-test-invoke-and-not-found#task#14.

Cause 2 of 5: GetStage reports no method setting for the method the unthrottled patch addressed.
AWS: recorded behavior only — the recorded 200 read carries methodSettings["test/GET"] with throttlingBurstLimit -1 and throttlingRateLimit -1.0 as above.
Emulator: 200 whose methodSettings has no "test/GET" entry at all, and the replay's poll for throttlingBurstLimit == -1 never settles (`poll: not in [-1] after 30 attempt(s)`).
Why: the stage's effective per-method configuration is unreadable, so a client cannot verify (or discover) that a method is unthrottled.
Fix: the Stage state's method-setting storage plus GetStage's methodSettings projection (follows cause 1, but must be re-replayed in its own right).
Traces: apigateway-stage-method-settings-unthrottled-mock-method-test-invoke-and-not-found#task#15.

Cause 3 of 5: UpdateStage's 200 response keys methodSettings by a canonicalised resource path instead of the key AWS reports for the patch path as written.
AWS: recorded behavior only — a patch addressing the method-setting path whose resource-path segment is written in escaped form answers 200 with the methodSettings key `~1pets/GET` (the escape preserved), carrying cacheDataEncrypted false, cacheTtlInSeconds 300, cachingEnabled false, dataTraceEnabled false, metricsEnabled false, requireAuthorizationForCacheControl true, throttlingRateLimit 100.0, unauthorizedCacheControlHeaderStrategy SUCCEED_WITH_RESPONSE_HEADER.
Emulator: 200 whose map has no `~1pets/GET` key and instead an extra `pets/GET` key with the same settings — the key has been rewritten.
Why: the method-settings map is addressed BY key; a client that reads back the key it patched (or that keys its own config off the response) finds its entry missing and an unexpected one present, and two settings that AWS keeps distinct are folded together.
Fix: UpdateStage's method-setting path parser — report the key as AWS reports it for the submitted path rather than canonicalising it.
Traces: apigateway-stage-method-settings-duplicate-spelling#task#7 (cluster also covers apigateway-stage-method-settings-path-grammar#task#7, #task#8, #task#9, #task#10, #task#12).

Cause 4 of 5: GetStage collapses two distinct method-settings keys into one.
AWS: recorded behavior only — the recorded 200 carries BOTH `/pets/GET` (throttlingBurstLimit 50) and `~1pets/GET` (throttlingRateLimit 100.0) as separate entries of the same stage's methodSettings map.
Emulator: 200 with neither key, and a single `pets/GET` entry instead (throttlingBurstLimit 5000, throttlingRateLimit 100.0).
Why: the two settings AWS keeps apart are merged, so one of the two patches a client applied is silently lost and the reported limits are neither of the two it set.
Fix: the Stage state's method-setting key identity plus GetStage's projection.
Traces: apigateway-stage-method-settings-duplicate-spelling#task#9 (cluster also covers #task#11).

Cause 5 of 5: GetStage reports the same rewritten key on the path-grammar trajectory, where the settings carry a cache/logging configuration.
AWS: recorded behavior only — the recorded 200 carries methodSettings[`~1pets/GET`] = {cacheDataEncrypted true, cacheTtlInSeconds 60, cachingEnabled true, dataTraceEnabled false, loggingLevel OFF, metricsEnabled true, requireAuthorizationForCacheControl false, throttlingRateLimit 100.0, unauthorizedCacheControlHeaderStrategy FAIL_WITH_403}.
Emulator: 200 with no `~1pets/GET` key and an extra `pets/GET` entry holding those values.
Why: as cause 3 — the key a client patched is not the key it can read back, on the read path this time.
Fix: GetStage's methodSettings projection, sharing the key identity fixed in causes 3-4.
Traces: apigateway-stage-method-settings-path-grammar#task#11.

Note on values: the api ids, deployment ids and stage names in these recordings are per-run values and are not part of the requirement. The requirements are structural: the methodSettings key AWS reports for a submitted method-setting patch path, the independence of two keys that differ only in that spelling, and the acceptance and round-tripping of the unthrottled throttling sentinel.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 2023 steps, 1938 passed, 85 mismatched. All five clusters report verdict=mismatch, stub_path="", owned_by=[].

Cause 1 — UpdateStage, fixture apigateway-stage-method-settings-unthrottled-mock-method-test-invoke-and-not-found, step task#14: expected_status 200 / actual_status 400. detail: status: expected 200, got 400; $.cacheClusterEnabled (False), $.cacheClusterStatus ('NOT_AVAILABLE'), $.createdDate, $.deploymentId, $.lastUpdatedDate, $.stageName ('dev'), $.tracingEnabled (False) and $.methodSettings ({'test/GET': {'cacheDataEncrypted': False, 'cacheTtlInSeconds': 300, 'cachingEnabled': False, 'dataTraceEnabled': False, 'metricsEnabled': False, 'requireAuthorizationForCacheControl': True, 'throttlingBurstLimit': -1, 'throttlingRateLimit': -1.0, 'unauthorizedCacheControlHeaderStrategy': 'SUCCEED_WITH_RESPONSE_HEADER'}}) all reported missing.

Cause 2 — GetStage, same fixture, step task#15: expected 200 / actual 200. detail: $.methodSettings.test/GET: missing (expected the settings map above, got None); $.methodSettings.test/GET.throttlingBurstLimit: poll: not in [-1] after 30 attempt(s) (expected -1, got None).

Cause 3 — UpdateStage, fixture apigateway-stage-method-settings-duplicate-spelling, step task#7: expected 200 / actual 200. detail: $.methodSettings.~1pets/GET: missing (expected {'cacheDataEncrypted': False, 'cacheTtlInSeconds': 300, 'cachingEnabled': False, 'dataTraceEnabled': False, 'metricsEnabled': False, 'requireAuthorizationForCacheControl': True, 'throttlingBurstLimit': <shaped-not-asserted>, 'throttlingRateLimit': 100.0, 'unauthorizedCacheControlHeaderStrategy': 'SUCCEED_WITH_RESPONSE_HEADER'}, got None); $.methodSettings.pets/GET: extra (expected None, got the same settings with throttlingBurstLimit 5000). Cluster members (6): apigateway-stage-method-settings-duplicate-spelling#task#7, apigateway-stage-method-settings-path-grammar#task#7, #task#8, #task#9, #task#10, #task#12.

Cause 4 — GetStage, fixture apigateway-stage-method-settings-duplicate-spelling, step task#9 (cluster also covers task#11): expected 200 / actual 200. detail: $.methodSettings./pets/GET: missing (expected settings with throttlingBurstLimit 50, got None); $.methodSettings.~1pets/GET: missing (expected settings with throttlingRateLimit 100.0, got None); $.methodSettings.pets/GET: extra (expected None, got throttlingBurstLimit 5000 / throttlingRateLimit 100.0).

Cause 5 — GetStage, fixture apigateway-stage-method-settings-path-grammar, step task#11: expected 200 / actual 200. detail: $.methodSettings.~1pets/GET: missing (expected {'cacheDataEncrypted': True, 'cacheTtlInSeconds': 60, 'cachingEnabled': True, 'dataTraceEnabled': False, 'loggingLevel': 'OFF', 'metricsEnabled': True, 'requireAuthorizationForCacheControl': False, 'throttlingRateLimit': 100.0, 'unauthorizedCacheControlHeaderStrategy': 'FAIL_WITH_403'}, got None); $.methodSettings.pets/GET: extra (expected None, got those values with throttlingBurstLimit 5000).

Model evidence: smithy_query query=operation UpdateStage — PATCH /restapis/{restApiId}/stages/{stageName}, code 200, input patchOperations: ListOfPatchOperation, output Stage with methodSettings: MapOfMethodSettings. smithy_query query=shape MapOfMethodSettings/MethodSetting — map of String -> MethodSetting; throttlingBurstLimit: Integer, throttlingRateLimit: Double, no range constraint on either.

## Steps to Reproduce
1. Point a boto3 `apigateway` client at the emulator, create a REST API with a /pets GET method, deploy it and create a stage.
2. Cause 1: `update_stage(restApiId=api_id, stageName=stage, patchOperations=[{'op':'replace','path':'/test/GET/throttling/burstLimit','value':'-1'},{'op':'replace','path':'/test/GET/throttling/rateLimit','value':'-1'}])` (the unthrottled sentinel) — observe HTTP 400; AWS answers 200 with the stage and methodSettings['test/GET'] holding -1 / -1.0.
3. Cause 2: `get_stage(...)` afterwards — methodSettings has no 'test/GET' entry at all (and never gains one over 30 polls).
4. Cause 3: `update_stage(..., patchOperations=[{'op':'replace','path':'/~1pets/GET/throttling/rateLimit','value':'100'}])` — observe the 200 response keys the setting as 'pets/GET'; AWS keys it '~1pets/GET'.
5. Cause 4: apply one method setting under '/pets/GET' and another under '/~1pets/GET' on the same stage, then `get_stage(...)` — the emulator reports one merged 'pets/GET' entry; AWS reports both keys as separate entries with their own values.
6. Cause 5: on the path-grammar trajectory (caching/logging settings patched through the escaped path), `get_stage(...)` again reports 'pets/GET' instead of '~1pets/GET'.
7. Full scenarios: replay fixture apigateway-stage-method-settings-unthrottled-mock-method-test-invoke-and-not-found steps task#14, task#15; fixture apigateway-stage-method-settings-duplicate-spelling steps task#7, task#9; fixture apigateway-stage-method-settings-path-grammar step task#11.
