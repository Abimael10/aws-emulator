Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none
Issue-Key: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none|_poll_until#41|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: d38a11298534a5b4ed5fe1ad46400f3e9411729b

# apigateway__cloudwatch__lambda: a REST stage publishes no AWS/ApiGateway metrics at all — Count under {ApiName, Stage} has zero datapoints, so the cache hit/miss metric trajectory is never gradeable

> _Note: this issue was identified against the code base at commit `d38a11298534a5b4ed5fe1ad46400f3e9411729b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
One net-new root cause in this service group.

Cause 1 of 1: requests served by a deployed API Gateway REST stage emit no AWS/ApiGateway metric data, so every stage-level series (Count, and the cache series CacheHitCount/CacheMissCount) is empty.
Failing check id: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none.

AWS: recorded behavior only — CloudWatch metric publication is not expressed in any Smithy model (smithy_query on the apigateway model exposes CreateStage/UpdateStage/FlushStageCache and the methodSettings members, but no metric emission contract), so the requirement comes from the recording and from the reference the check itself cites, https://docs.aws.amazon.com/apigateway/latest/developerguide/api-gateway-metrics-and-dimensions.html. In the namespace AWS/ApiGateway, a REST API stage publishes Count at 1-minute granularity under the dimension set {ApiName, Stage} for EVERY request the stage answers, with no opt-in required (stage detailed metrics / methodSettings metricsEnabled gate only the per-METHOD dimension set {ApiName, Method, Resource, Stage}). A stage with cacheClusterEnabled additionally publishes CacheHitCount and CacheMissCount under the same {ApiName, Stage} set, and a stage with no cache publishes NEITHER cache series while still publishing Count. The requirement is the mapping request -> datapoint for the stage that served it; the particular ApiName, stage name and request counts are per-run values.

Emulator: the stage serves the requests correctly — the check gets 200s from both stages, observes a cold-cache miss, a subsequent hit, and a FlushStageCache-forced miss, and reaches its first metric barrier — but CloudWatch holds no series for it. GetMetricStatistics(Namespace='AWS/ApiGateway', MetricName='Count', Dimensions=[{'Name':'ApiName','Value':<api name>},{'Name':'Stage','Value':'ctl'}], Period=60, Statistics=['Sum']) returns an EMPTY Datapoints list: the replay reports "Count Sum [{'Name': 'ApiName', 'Value': 'sim2real-xsvc-cachem-8ee0ab6fd2fe'}, {'Name': 'Stage', 'Value': 'ctl'}] reaching 3 not met within 300s. Last result: (0, 0)" — sum 0 over 0 datapoints after 3 requests the stage demonstrably answered 200. No status or error code diverges here: both GetMetricStatistics and the stage invoke answer 200; what diverges is that nothing was ever published, so the later barriers (Count on the cached stage, CacheMissCount, CacheHitCount, and the GetMetricData cross-check) are unreachable too.

Why: AWS/ApiGateway Count is the only count of traffic a REST API offers, and CacheHitCount/CacheMissCount are the only evidence a stage cache is effective. A client that alarms on request volume or 5XX rate, dashboards an API, sizes a cache cluster from its hit ratio, or autoscales on Count sees a permanently idle API: alarms sit in INSUFFICIENT_DATA forever, dashboards are blank, and a cache regression is invisible. Nothing distinguishes "the API served nothing" from "the API served everything".

Fix: the API Gateway stage request path — the component that answers a deployed stage's invoke must emit an AWS/ApiGateway datapoint per request into the CloudWatch metric store under {ApiName, Stage} (Count always; CacheHitCount/CacheMissCount only while the stage's cacheClusterEnabled is true and only for cache-eligible methods), alongside the existing per-method emission gated by methodSettings metricsEnabled. The CloudWatch read side needs no change. Related open issues that do NOT carry this check's replay key and do not cover it: 20261109T152222Z-apigateway-cloudwatch-aws-apigateway-count-metrics-are-never.md (same seam, filed for service apigateway__cloudwatch, and it owns two other checks in this replay), 20261109T152222Z-apigateway-cloudwatch-lambda-updatestage-refuses-a-real-method.md and 20261110T162302Z-apigateway-cloudwatch-lambda-a-rest-api-5xx-alarm-never-transitions-ok.md (which own the other three failures under this heading). This check is the only one in the replay that exercises the cache series, so closing it requires CacheHitCount/CacheMissCount as well as Count.

Traces: apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 907/1226 passed (74.0%); 268 mismatched, 51 errored, 0 skipped; bootstrap_signal false (the emulator answered throughout). Per-service apigateway__cloudwatch__lambda 1/5 (20%), 4 failed; the other three are marked OWNED by open issues and are excluded here.

[mismatch] apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none
  File "...obs_apigateway_1_cache_hit_miss_metrics_track_flush_uncached_stage_has_none.py", line 227, in _await_sum
    got, _n = _poll_until(lambda: _gms(ctx, ns, metric, dims, start), ...)
  AssertionError: Count Sum [{'Name': 'ApiName', 'Value': 'sim2real-xsvc-cachem-8ee0ab6fd2fe'}, {'Name': 'Stage', 'Value': 'ctl'}] reaching 3 not met within 300s. Last result: (0, 0)

What the emulator DID do correctly before that point (so these are not the defect): cacheClusterStatus reached AVAILABLE for a 0.5 GB cluster; GetStage reported methodSettings['c/GET'].cachingEnabled == True on the cached stage and cacheClusterEnabled == False with empty methodSettings on the control stage; GET /c on the cached stage returned 200 'inv-<request id>' and missed cold, then hit; FlushStageCache forced a subsequent miss; three GETs on the uncached control stage each returned 200 with three distinct bodies (so all three reached Lambda).

Expected (AWS): after those requests, GetMetricStatistics over Namespace 'AWS/ApiGateway', MetricName 'Count', Dimensions {ApiName=<api>, Stage='ctl'}, Period 60, Statistic Sum returns datapoints summing to 3 (one per request the stage answered); the same read for Stage='cached' sums to the client-measured hits+misses, and CacheMissCount / CacheHitCount for that stage sum to the measured misses and hits respectively, with GetMetricData agreeing; the uncached 'ctl' stage returns 0 datapoints for CacheHitCount/CacheMissCount while still returning Count.
Actual (emulator): (sum 0, 0 datapoints) for Count on the control stage, held for the full 300 s barrier — the series does not exist. The cached-stage Count, CacheMissCount, CacheHitCount, the GetMetricData cross-check, the AWS/Lambda Invocations agreement (misses + control GETs) and the effect-absent hold on the uncached stage are all left ungraded.

Smithy consulted: the apigateway model carries no metric-publication contract (metric emission is not a modelled operation), so the requirement is recorded behavior plus the AWS reference the check cites: api-gateway-metrics-and-dimensions (AWS/ApiGateway; dimension sets {ApiName}, {ApiName, Stage}, {ApiName, Method, Resource, Stage}; metrics Count, 4XXError, 5XXError, Latency, IntegrationLatency, CacheHitCount, CacheMissCount).

Bootstrap gate: get_bootstrap_status() reports pending == [] — apigateway, cloudwatch and lambda are all stood up in this checkout (this service group passes 1 of 5 checks and the whole suite passes 74%), so this is a fidelity finding, not a bootstrap gap.

## Steps to Reproduce
Boot the emulator and point apigateway, lambda and cloudwatch clients at its data listener.
1. CreateFunction with a handler that answers 200 with a body containing its own aws_request_id (so each response identifies the invocation that produced it).
2. CreateRestApi; create resource /c and /ready with ANY/GET -> AWS_PROXY integrations onto that function; CreateDeployment to stage 'cached' with cacheClusterEnabled=true and cacheClusterSize='0.5', and to stage 'ctl' with no cache.
3. UpdateStage on 'cached' to set methodSettings for 'c/GET' with cachingEnabled=true; wait for cacheClusterStatus == 'AVAILABLE'.
4. Send GET <ctl stage>/c three times; each must answer 200 with a distinct body (no cache).
5. THE GRADED READ: cloudwatch.GetMetricStatistics(Namespace='AWS/ApiGateway', MetricName='Count', Dimensions=[{'Name':'ApiName','Value':<api name>},{'Name':'Stage','Value':'ctl'}], StartTime=<before step 4>, EndTime=<now+3min>, Period=60, Statistics=['Sum']).
   EXPECTED (AWS): one or more datapoints whose Sum totals 3.
   OBSERVED: Datapoints == [] (sum 0 over 0 points), unchanged for 300 s.
6. Send GET <cached stage>/c until a body repeats (a cache hit), then FlushStageCache and GET again until a fresh body appears (a forced miss). Read Count, CacheMissCount and CacheHitCount for {ApiName=<api>, Stage='cached'}.
   EXPECTED (AWS): Count == hits+misses, CacheMissCount == misses, CacheHitCount == hits, and GetMetricData agrees with GetMetricStatistics.
   OBSERVED: all three series are empty.
7. Read CacheHitCount/CacheMissCount for {ApiName=<api>, Stage='ctl'}. EXPECTED (AWS): 0 datapoints (an uncached stage publishes no cache series) — this one leg already matches, and must keep matching once Count is published.
8. Re-replay apigateway__cloudwatch__lambda.obs-apigateway-1-cache-hit-miss-metrics-track-flush-uncached-stage-has-none.
