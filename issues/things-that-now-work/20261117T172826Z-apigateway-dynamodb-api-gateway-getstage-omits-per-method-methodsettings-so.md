Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-cache-ttl-expiry-and-flush-eviction apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-unauthorized-cache-control-strategies
Issue-Key: apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-cache-ttl-expiry-and-flush-eviction|run#19|KeyError|assert ms["~1short/GET"]["cacheTtlInSeconds"] == _SHORT_TTL, f"GetStage methodSettings {ms!r}"
Issue-Key: apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-unauthorized-cache-control-strategies|run#20|KeyError|ms = apigw.get_stage(restApiId=api_id, stageName=stage)["methodSettings"]["~1count/GET"]
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# API Gateway: GetStage omits per-method methodSettings, so a stage's cache and logging overrides cannot be read back

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStage

## Description
Cause 1 of 1: a stage's per-method settings are not reported by GetStage.
AWS: method settings are applied to a stage with UpdateStage patch operations whose path is /{resourcePath}/{httpMethod}/{setting} (the resource path escaped with ~1, e.g. /~1short/GET/caching.ttlInSeconds) or by CreateDeployment/UpdateStage with methodSettings. GetStage then answers a methodSettings map KEYED by '<escaped resource path>/<httpMethod>' — e.g. '~1short/GET' — whose value carries cacheTtlInSeconds, cachingEnabled, loggingLevel, dataTraceEnabled and the other members of MethodSetting. The '*/*' wildcard key is a separate entry and does not replace the per-method one.
Emulator: GetStage's methodSettings map has no entry for the per-method key: indexing ms['~1short/GET'] and ...['methodSettings']['~1count/GET'] raises KeyError, so the applied cache TTL / caching strategy is unreadable.
Why: a client that sets a per-method cache TTL or an unauthorized-cache-control strategy and then reads the stage back to confirm it (the only way to confirm it) finds nothing, and any code that iterates methodSettings to drive cache flushing or logging configuration sees an empty or wildcard-only map. Two behavioral checks cannot get past this to grade cache expiry/flush eviction or cache-control strategies at all.
Fix: the API Gateway stage state must store method settings under their '<escaped resource path>/<httpMethod>' key when applied via UpdateStage patch operations or methodSettings, and the GetStage/UpdateStage serializers must emit that map.
Traces: apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-cache-ttl-expiry-and-flush-eviction, apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-unauthorized-cache-control-strategies

## Evidence
From the behavioral replay:
- apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-cache-ttl-expiry-and-flush-eviction: 'assert ms["~1short/GET"]["cacheTtlInSeconds"] == _SHORT_TTL, f"GetStage methodSettings {ms!r}"' → KeyError: '~1short/GET'.
- apigateway__dynamodb.deep-apigateway-1-apigw-methodsetting-unauthorized-cache-control-strategies: 'ms = apigw.get_stage(restApiId=api_id, stageName=stage)["methodSettings"]["~1count/GET"]' → KeyError: '~1count/GET'.
Expected: GetStage returns a methodSettings entry under that exact key carrying the settings just applied (cacheTtlInSeconds, cachingEnabled, and the cache-control strategy members).

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the two check ids in trace_ids.
3. Manual repro: create a REST API with a /short resource and a GET method, deploy it to a stage, then call UpdateStage with patchOperations [{'op':'replace','path':'/~1short/GET/caching.enabled','value':'true'},{'op':'replace','path':'/~1short/GET/caching.ttlInSeconds','value':'<ttl>'}].
4. Call GetStage and read methodSettings — AWS returns an entry keyed '~1short/GET' with cachingEnabled/cacheTtlInSeconds; the emulator's map has no such key.
