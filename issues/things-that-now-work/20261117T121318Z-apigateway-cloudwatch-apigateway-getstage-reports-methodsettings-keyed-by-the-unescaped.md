Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch.obs-apigateway-3-per-method-metrics-only-for-the-enabled-method
Issue-Key: apigateway__cloudwatch.obs-apigateway-3-per-method-metrics-only-for-the-enabled-method|run#19|AssertionError|assert set(settings) == {"~1ok/GET"} and settings["~1ok/GET"].get("metricsEnabled") is True, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# apigateway: GetStage reports methodSettings keyed by the unescaped resource path, so the key UpdateStage's '/~1<resource>/<METHOD>/...' patch created cannot be found

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetStage

## Description
Cause 1 of 1: GetStage returns the per-method settings map keyed by the raw resource path instead of the JSON-Patch-escaped key the stage-settings path uses.
AWS: Smithy (apigateway 2015-07-09) models Stage$methodSettings as MapOfMethodSettings whose documentation states: "Keys (designated as /{method_setting_key} below) are method paths defined as {resource_path}/{http_method} for an individual method override, or /*/* for overriding all methods in the stage." UpdateStage takes JSON-Patch operations (RFC 6901), where a '/' inside a path segment is written '~1'; enabling detailed metrics on resource /ok method GET is therefore the patch path '/~1ok/GET/metrics/enabled', and GetStage reports that override under the key '~1ok/GET' — the escaped form, verbatim. Only the methods actually overridden appear: no '*/*' entry and no entry for a method that was never patched. Recorded behavior only for the exact key spelling (the model documents the shape of the key, not the escaping).
Emulator: after the same UpdateStage patch, GetStage answers methodSettings={'ok/GET': {...metricsEnabled: true...}} — the key has been un-escaped ('~1' dropped), so the escaped key the client wrote and will read back by does not exist. The settings values themselves (metricsEnabled true, the throttling/caching defaults) are correct; only the key form diverges.
Why: a client enables detailed metrics with one path and then reads the map by that same path. With the key rewritten, every lookup of '~1<resource>/<METHOD>' misses, so tooling concludes metrics are off for the method it just enabled, and a subsequent patch targeting the escaped key cannot be correlated with the entry already present. The key is a value the client supplied, not a server-generated id.
Fix: the GetStage (and UpdateStage) serializer/state for Stage.methodSettings — the map key must be stored and emitted in the same escaped '{resource_path}/{http_method}' form the patch path carries, not the decoded path.
Traces: apigateway__cloudwatch.obs-apigateway-3-per-method-metrics-only-for-the-enabled-method.

## Evidence
Expected: GetStage reports exactly one method setting, key '~1ok/GET' with metricsEnabled=True (no '*/*' and no '~1other/GET').
Actual: "AssertionError: expected GetStage to report exactly one method setting, '~1ok/GET' with metricsEnabled=True (no '*/*' and no '~1other/GET'), got methodSettings={'ok/GET': {'metricsEnabled': True, 'dataTraceEnabled': False, 'throttlingBurstLimit': 5000, 'throttlingRateLimit': 10000.0, 'cachingEnabled': False, 'cacheTtlInSeconds': 300, 'cacheDataEncrypted': False, 'requireAuthorizationForCacheControl': True, 'unauthorizedCacheControlHeaderStrategy': 'SUCCEED_WITH_RESPONSE_HEADER'}}" — the resource name 'ok' and method 'GET' come from the request; the divergence is the missing '~1' escape in the key.

## Steps to Reproduce
1. CreateRestApi, create a resource '/ok' with a GET method and an integration, CreateDeployment and a stage.
2. UpdateStage(restApiId, stageName, patchOperations=[{op:'replace', path:'/~1ok/GET/metrics/enabled', value:'true'}]).
3. GetStage(restApiId, stageName) and inspect methodSettings: AWS keys the entry '~1ok/GET'; the emulator keys it 'ok/GET'.
