Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-control-invalidation-rejected-403-when-unauthorized apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none
Issue-Key: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-control-invalidation-rejected-403-when-unauthorized|run#21|AssertionError|assert got == {"locked": (True, True, "FAIL_WITH_403"),
Issue-Key: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none|run#19|AssertionError|assert cached.get("methodSettings", {}).get("c/GET", {}).get("cachingEnabled") is True, (
Issue-Commit: 027692653b37f8059828c0218f5349555bde1430

# API Gateway stage methodSettings are keyed by the escaped patch-path spelling (`~1<resource>/GET`) instead of AWS's `{resource_path}/{http_method}` method path (1 cause)

> _Note: this issue was identified against the code base at commit `027692653b37f8059828c0218f5349555bde1430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStage

## Description
Cause 1 of 1: the Stage read/write surface returns the `methodSettings` map keyed by the JSON-Pointer-escaped patch-path spelling, not by AWS's method-path key, so no client can look up the settings it just applied.

AWS: Smithy (com.amazonaws.apigateway#BackplaneControlService): `Stage$methodSettings` targets `MapOfMethodSettings` (key `String`, value `MethodSetting`), documented as "A map that defines the method settings for a Stage resource. Keys ... are method paths defined as `{resource_path}/{http_method}` for an individual method override, or `/*/*` for overriding all methods in the stage." So for resource path `/c` and verb `GET` the response key is exactly `c/GET`; for `/locked` + `GET` it is `locked/GET`. The `~1` escape sequence belongs ONLY to `UpdateStage`'s `patchOperations[].path` (e.g. `/~1c~1GET/caching/enabled`), where the resource path's slashes must be escaped — it is never part of the response map key. The `MethodSetting` value carries the patched members plus the model's defaults for every untouched member (`metricsEnabled`, `loggingLevel`, `dataTraceEnabled`, `throttlingBurstLimit`, `throttlingRateLimit`, `cachingEnabled`, `cacheTtlInSeconds`, `cacheDataEncrypted`, `requireAuthorizationForCacheControl`, `unauthorizedCacheControlHeaderStrategy`).

Emulator: the keys come back with the leading slash escaped and the remaining separator left literal — the replayed stages report `~1c/GET`, `~1ready/GET`, `~1open/GET` and `~1locked/GET`. The `MethodSetting` VALUES are correct (the patched `cachingEnabled`/`requireAuthorizationForCacheControl`/`unauthorizedCacheControlHeaderStrategy` are all present with sane defaults alongside), so the only divergence is the key spelling. The general rule: the emulator echoes the patch path's escaped form as the map key instead of normalising it back to `{resource_path}/{http_method}`. Both failing checks die on a precondition that reads the settings back by AWS's key and finds nothing under it (`methodSettings` lookup of `c/GET` / `locked/GET` returns None).

Why: a client cannot read back the per-method throttling, caching and cache-invalidation-authorization configuration it just wrote. Anything that reconciles desired vs actual stage state — CloudFormation/CDK drift detection, an idempotent re-apply, a console render, or a test that asserts caching is on before exercising cache hit/miss — concludes the setting was never applied and either re-applies forever or reports permanent drift. A client that keys off `/*/*` plus a per-method override cannot tell which override applies.

Fix: the stage state's method-settings keying plus the `GetStage`/`UpdateStage` serializers in the apigateway crate — unescape the patch path and store/emit each entry under the normalised `{resource_path}/{http_method}` key (with `/*/*` preserved as-is). Related open issues that do NOT cover this: `20261107T204751Z-apigateway-api-gateway-stage-method-settings-duplicate-resource-path.md` (its Cause 2 is the wire-corpus sighting of a MISSING normalised key on GetStage) and `20261104T130732Z-apigateway-updatestage-returns-an-extra-methodsettings-entry-keyed-by.md` (an EXTRA un-normalised key on the UpdateStage response). Neither is marked as owning these behavioral checks, and neither names the `~1`-prefixed key actually served here.

Traces: the two failing checks below.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none
  AssertionError: precondition: GET /c caching must be ON, methodSettings {'~1c/GET': {'metricsEnabled': False, 'dataTraceEnabled': False, 'throttlingBurstLimit': 5000, 'throttlingRateLimit': 10000.0, 'cachingEnabled': True, 'cacheTtlInSeconds': 300, 'cacheDataEncrypted': False, 'requireAuthorizationForCacheControl': True, 'unauthorizedCacheControlHeaderStrategy': 'SUCCEED_WITH_RESPONSE_HEADER'}, '~1ready/GET': {...}}
  Expected key: 'c/GET'. Actual key: '~1c/GET'.

[mismatch] apigateway__cloudwatch__lambda.obs-apigateway-1-cache-control-invalidation-rejected-403-when-unauthorized
  AssertionError: precondition: the two methods must differ ONLY in requireAuthorizationForCacheControl; methodSettings {'~1open/GET': {...'cachingEnabled': True, 'requireAuthorizationForCacheControl': False, 'unauthorizedCacheControlHeaderStrategy': 'FAIL_WITH_403'}, '~1ready/GET': {...}, '~1locked/GET': {...'cachingEnabled': True, 'requireAuthorizationForCacheControl': True, 'unauthorizedCacheControlHeaderStrategy': 'FAIL_WITH_403'}}
  Expected keys: 'open/GET', 'ready/GET', 'locked/GET'. Actual keys: '~1open/GET', '~1ready/GET', '~1locked/GET'.

The requirement is the key FORM — `{resource_path}/{http_method}` with slashes unescaped — not the particular resource names, verbs or limit values from these runs, which come from the request.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateRestApi; add a resource (e.g. `/c`) with a GET method and an integration; CreateDeployment with a stage and a cache cluster.
3. UpdateStage with patchOperations that set the method's settings under the escaped patch path, e.g. op=replace path=/~1c~1GET/caching/enabled value=true, and op=replace path=/~1c~1GET/caching/ttlInSeconds value=300.
4. Call GetStage and inspect `methodSettings`: the entry is keyed `~1c/GET`. AWS keys it `c/GET`.
5. Or replay the two checks named in trace_ids.
