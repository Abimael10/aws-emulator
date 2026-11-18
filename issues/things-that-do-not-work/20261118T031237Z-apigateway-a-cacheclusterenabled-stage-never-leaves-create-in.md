Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.flush-caches-bypass-ttl
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.flush-caches-bypass-ttl|_poll_until#24|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s, last value={last!r}")
Issue-Commit: 8b0a0326e7d91c3197ca4e29d037f7bc2fabea56

# apigateway: a cacheClusterEnabled stage never leaves CREATE_IN_PROGRESS, so GetStage.cacheClusterStatus has no terminal value

> _Note: this issue was identified against the code base at commit `8b0a0326e7d91c3197ca4e29d037f7bc2fabea56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStage

## Description
One net-new root cause in the apigateway emulator, grounded in a failing behavioral check that no OPEN issue carries the replay key for. Note: the resolved issue 20261107T222601Z-apigateway-2-net-new-causes-a-deployed-stage.md carried this same check as its Cause 2 and claimed to fix it; that issue is now in the resolved folder, yet the check still fails on exactly the same diverging value, so this cause is still live and is re-filed here. It is not a refusal — GetStage answers 200, it just never reports a terminal cacheClusterStatus. The related OPEN issues 20261111T020136Z-apigateway-flushstageauthorizerscache-s-ttl-bypass-is-never-observable.md and 20261116T020413Z-apigateway-the-stage-response-cache-s-stale-until.md do NOT carry this check's replay key and do not cover it.

Cause 1 of 1: a REST stage created with cacheClusterEnabled=true keeps reporting cacheClusterStatus 'CREATE_IN_PROGRESS' forever, so the stage response cache never becomes usable and nothing downstream of it (per-method caching settings, FlushStageCache's 202, FlushStageAuthorizersCache leaving the response cache cluster untouched) can be exercised.
Failing check: apigateway.flush-caches-bypass-ttl

AWS: CreateStage with cacheClusterEnabled=true and cacheClusterSize="0.5" provisions a response cache cluster for the stage. Smithy (com.amazonaws.apigateway#BackplaneControlService): Stage.cacheClusterStatus targets enum CacheClusterStatus with values CREATE_IN_PROGRESS, AVAILABLE, DELETE_IN_PROGRESS, NOT_AVAILABLE, FLUSH_IN_PROGRESS; Stage.cacheClusterSize targets enum CacheClusterSize with values 0.5, 1.6, 6.1, 13.5, 28.4, 58.2, 118, 237. Behaviorally (recorded behavior only — the model names the enum values, not the transition rules): the status starts at CREATE_IN_PROGRESS and then LEAVES it for a terminal value — AVAILABLE when provisioning succeeds, NOT_AVAILABLE when it fails — and only once it is AVAILABLE is the stage's per-method response caching in effect; FlushStageCache moves it to FLUSH_IN_PROGRESS and back to AVAILABLE. The requirement is that a terminal value is reached and read back by GetStage; how many seconds provisioning takes, and the concrete api id / stage name, are not requirements.
Emulator: GetStage(restApiId, stageName) answers 200 but keeps reporting cacheClusterStatus 'CREATE_IN_PROGRESS' for the whole poll window and never reports AVAILABLE or NOT_AVAILABLE. The replay detail is `AssertionError: condition not met within 1.0s, last value='CREATE_IN_PROGRESS'`, raised from the check's _poll_until over GetStage(...)['cacheClusterStatus']. The 1.0 s window is the floor the check's _bounded() falls back to because the earlier steps of the same check — getting the backend MOCK stage to answer through its stage URL — had already consumed its budget; the value asserted on, and the value that diverges, is the status itself, which is the non-terminal CREATE_IN_PROGRESS on every read. No tool was missing and no TLS or name-resolution failure of the runner is reported: the asserted value is one the emulator serves.
Why: a client that enables a stage cache and waits for cacheClusterStatus to settle before shifting traffic (the documented way to know the cache is live) waits forever, and every subsequent cache behavior — a per-method cachingEnabled/cacheTtlInSeconds override taking effect, FlushStageCache invalidating an entry whose TTL is still live, FlushStageAuthorizersCache not disturbing the response cache cluster — is unobservable and cannot be relied on.
Fix: the stage state that models the cache cluster lifecycle (CreateStage/UpdateStage must transition cacheClusterStatus out of CREATE_IN_PROGRESS to AVAILABLE, FlushStageCache must go FLUSH_IN_PROGRESS -> AVAILABLE, DeleteStage/disable must go DELETE_IN_PROGRESS), plus the GetStage serializer that projects it.
Traces: apigateway.flush-caches-bypass-ttl

## Evidence
Cause 1 — apigateway.flush-caches-bypass-ttl
  Expected: after CreateStage(restApiId=<api>, stageName='prod', deploymentId=<id>, cacheClusterEnabled=True, cacheClusterSize='0.5'), repeated GetStage(restApiId=<api>, stageName='prod')['cacheClusterStatus'] settles on a terminal CacheClusterStatus value — 'AVAILABLE' for a successful provision (or 'NOT_AVAILABLE' on failure) — after which per-method caching is in effect, FlushStageCache answers HTTP 202, and the cluster returns to 'AVAILABLE'.
  Actual: GetStage returns 200 with cacheClusterStatus 'CREATE_IN_PROGRESS' on every read; the check's poll never sees a terminal value and fails with `AssertionError: condition not met within 1.0s, last value='CREATE_IN_PROGRESS'` (raised at check_apigateway_flush_caches_bypass_ttl.py _poll_until, from the probe GetStage(...)['cacheClusterStatus']). Because the status never settles, the rest of the check (the '~1items/GET' methodSettings read of cachingEnabled=True / cacheTtlInSeconds=300, the stable cached response id, FlushStageCache -> 202, the post-flush fresh id inside the live 300 s TTL, and the two back-to-back FlushStageAuthorizersCache 202s) is never reached.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Re-replay the behavioral check apigateway.flush-caches-bypass-ttl.
3. Or by hand: CreateRestApi (endpointConfiguration types=[REGIONAL]); CreateResource pathPart='items' under the root resource; PutMethod GET authorizationType=NONE; PutIntegration type=HTTP_PROXY integrationHttpMethod=GET uri=<any reachable backend URL>.
4. CreateDeployment(restApiId=<api>), then CreateStage(restApiId=<api>, stageName='prod', deploymentId=<id>, cacheClusterEnabled=True, cacheClusterSize='0.5').
5. Poll GetStage(restApiId=<api>, stageName='prod') and read cacheClusterStatus. It stays 'CREATE_IN_PROGRESS' indefinitely instead of reaching 'AVAILABLE' (or 'NOT_AVAILABLE').
6. Then UpdateStage with patchOperations replace '/~1items/GET/caching/enabled'='true' and '/~1items/GET/caching/ttlInSeconds'='300', call FlushStageCache(restApiId, stageName='prod') (must be 202) and re-read cacheClusterStatus — it must pass through FLUSH_IN_PROGRESS back to 'AVAILABLE'.
