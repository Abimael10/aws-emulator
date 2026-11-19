Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch__lambda.metrics-elbv2-apigw-rest-api-lambda-authorizer-denied-403-counts-4xx-not-5xx
Issue-Key: apigateway__cloudwatch__lambda.metrics-elbv2-apigw-rest-api-lambda-authorizer-denied-403-counts-4xx-not-5xx|_poll_until#16|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: 5eee76838ae2b0ebdc35ca5fe4125e8c2661bc49

# apigateway__cloudwatch__lambda: a REST stage behind a Lambda TOKEN authorizer publishes no AWS/ApiGateway metrics — Count/4XXError/5XXError/Latency under {ApiName, Stage} have zero datapoints, so a 403 authorizer deny is never classified as a client error

> _Note: this issue was identified against the code base at commit `5eee76838ae2b0ebdc35ca5fe4125e8c2661bc49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
One net-new root cause in this service group.

Cause 1 of 1: requests answered by a deployed API Gateway REST stage emit no AWS/ApiGateway metric data at stage resolution, so the series that classify a Lambda TOKEN authorizer's 403 deny (Count, 4XXError, 5XXError, Latency under {ApiName, Stage}) never exist and the classification cannot be observed at all.
Failing check id: apigateway__cloudwatch__lambda.metrics-elbv2-apigw-rest-api-lambda-authorizer-denied-403-counts-4xx-not-5xx.

AWS: recorded behavior only — CloudWatch metric publication is not expressed by any modelled operation (neither the apigateway nor the cloudwatch Smithy model carries a metric-emission contract; only the read side, GetMetricStatistics/GetMetricData, is modelled), so the requirement comes from the recording and from the reference the check itself cites, https://docs.aws.amazon.com/apigateway/latest/developerguide/api-gateway-metrics-and-dimensions.html. In namespace AWS/ApiGateway a REST stage publishes, at 1-minute granularity and with no opt-in required, under the dimension set {ApiName, Stage}: Count — one datapoint contribution per request the stage ANSWERS, including requests the stage itself refuses before the integration runs; 4XXError — one per request answered with a 4xx; 5XXError — published and reading 0 when no 5xx occurred (the series exists, its Sum is 0); and Latency — SampleCount equal to the number of requests answered. A request refused by a CUSTOM (Lambda TOKEN) authorizer whose policy denies is a CLIENT error: the stage answers 403 with x-amzn-ErrorType: AccessDeniedException and body {"Message":"User is not authorized to access this resource with an explicit deny in an identity-based policy"}, and that request counts in Count and 4XXError and NOT in 5XXError. (Fixed text of that message is as quoted; nothing in it is per-run. The ApiName, stage name and the request counts are values from the request and from this run, not part of the requirement.) Detailed metrics / methodSettings metrics/enabled gate only the per-METHOD set {ApiName, Method, Resource, Stage}, never this stage-level set.

Emulator: the data plane behaves correctly up to the metric barrier — 5 allowed requests answered 200 'LAMBDA-200' and 7 denied requests each answered 403 with body {"Message":"User is not authorized to access this resource with an explicit deny in an identity-based policy"} and x-amzn-ErrorType: AccessDeniedException (those assertions passed). But CloudWatch holds no series for the stage. GetMetricStatistics(Namespace='AWS/ApiGateway', MetricName='Count', Dimensions=[{'Name':'ApiName','Value':<api name>},{'Name':'Stage','Value':'live'}], Period=60, Statistics=['Sum']) returns an EMPTY Datapoints list for the whole 240 s barrier: the replay reports "AssertionError: Count Sum [{'Name': 'ApiName', 'Value': 'sim2real-mrest-d2c352fb2a63'}, {'Name': 'Stage', 'Value': 'live'}] reaching 12 not met within 240s. Last result: (0, 0)" — sum 0 over 0 datapoints after 12 requests the stage demonstrably answered. No status or error code diverges on the read itself (GetMetricStatistics answers 200); what diverges is that nothing was ever published, which also leaves the 4XXError == 7, 5XXError == 0-over->0-datapoints, Latency SampleCount == 12 and the AWS/Lambda Invocations effect-absent legs (authorizer invoked 12 times, integration only 5) ungraded.

Why: a client relying on AWS's behavior cannot see API traffic or error classification at all. Alarms on Count, 4XXError, 5XXError or Latency sit in INSUFFICIENT_DATA forever; dashboards are blank; and because the 5XXError series is absent rather than present-and-zero, a client cannot distinguish "authorizer denials are correctly booked as client errors" from "no metrics are emitted" — a 5xx-rate alarm would never fire for a genuine integration failure either. The negative half of the contract (a deny must not count as 5XX) passes vacuously.

Fix: the execute-api / API Gateway stage request path — the component that answers a deployed stage's invoke must emit an AWS/ApiGateway datapoint per answered request into the CloudWatch metric store under {ApiName, Stage} (Count always; 4XXError for a 4xx answer, including an authorizer-deny 403 produced before the integration runs; 5XXError published with value 0 when the answer is not 5xx; Latency with one sample per request), independently of the per-method emission gated on methodSettings metrics/enabled. The CloudWatch read side needs no change. Related open issues that do NOT carry this check's replay key and do not cover it: 20261109T152222Z-apigateway-cloudwatch-aws-apigateway-count-metrics-are-never.md (same seam, filed for service apigateway__cloudwatch, owning two other checks), 20261115T232640Z-apigateway-cloudwatch-lambda-a-rest-stage-publishes-no.md (same seam for the cache series, owning the cache-hit/miss check), 20261109T152222Z-apigateway-cloudwatch-lambda-updatestage-refuses-a-real-method.md and 20261110T162302Z-apigateway-cloudwatch-lambda-a-rest-api-5xx-alarm-never-transitions-ok.md (owning the other failures under this heading). This check is the only one in the replay that grades the 4XXError/5XXError classification of an authorizer-denied 403 and the Latency SampleCount, so closing it requires those series, not Count alone.

Traces: apigateway__cloudwatch__lambda.metrics-elbv2-apigw-rest-api-lambda-authorizer-denied-403-counts-4xx-not-5xx.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 798/1019 passed (78.3%); 184 mismatched, 37 errored, 0 skipped; bootstrap_signal false (the emulator answered throughout). Per-service apigateway__cloudwatch__lambda 1/5 (20%), 4 failed; the other three are marked OWNED by open issues and are excluded here.

[mismatch] apigateway__cloudwatch__lambda.metrics-elbv2-apigw-rest-api-lambda-authorizer-denied-403-counts-4xx-not-5xx
  File "check_apigateway__cloudwatch__lambda_metrics_elbv2_apigw_rest_api_lambda_authorizer_denied_403_counts_4xx_not_5xx.py", line 213, in _await
    got, _ = _poll_until(lambda: _stat(ctx, metric, dims, start, statistic), ...)
  File "...", line 55, in _poll_until
    raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
  AssertionError: Count Sum [{'Name': 'ApiName', 'Value': 'sim2real-mrest-d2c352fb2a63'}, {'Name': 'Stage', 'Value': 'live'}] reaching 12 not met within 240s. Last result: (0, 0)

What the emulator DID do correctly before that point (so these are not the defect): 5 requests with Authorization: allow answered 200 with body b'LAMBDA-200'; 7 requests with Authorization: deny answered 403 with body b'{"Message":"User is not authorized to access this resource with an explicit deny in an identity-based policy"}' and header x-amzn-ErrorType: AccessDeniedException.

Expected (AWS), reading over the aligned minute window: GetMetricStatistics(Namespace='AWS/ApiGateway', MetricName='Count', Dimensions={ApiName=<api>, Stage='live'}, Period=60, Statistics=['Sum']) returns datapoints summing to 12 (5 allowed + 7 denied); the same read for 4XXError sums to 7; 5XXError returns at least one datapoint and sums to 0; Latency returns SampleCount 12; and AWS/Lambda Invocations shows 12 for the authorizer function and 5 for the integration function.
Actual (emulator): Count -> (sum 0, 0 datapoints) held for the full 240 s barrier; the series does not exist, so 4XXError, 5XXError, Latency and the Lambda Invocations cross-check are never evaluated.

Smithy consulted: no metric-publication contract exists in the models (metric emission is not a modelled operation; only the CloudWatch read operations are), so the requirement is recorded behavior plus the AWS reference the check cites: api-gateway-metrics-and-dimensions (AWS/ApiGateway; dimension sets {ApiName}, {ApiName, Stage}, {ApiName, Method, Resource, Stage}; metrics Count, 4XXError, 5XXError, Latency, IntegrationLatency, CacheHitCount, CacheMissCount).

Bootstrap gate: get_bootstrap_status() reports pending == [] — no service in this checkout is awaiting bootstrap, so this is a fidelity finding, not a bootstrap gap.

## Steps to Reproduce
Boot the emulator and point apigateway, lambda, iam, sts and cloudwatch clients at its data listener.
1. CreateFunction twice (python3.12) with one handler that (a) returns a TOKEN authorizer policy with Effect=Allow when authorizationToken == 'allow' and Effect=Deny otherwise, and (b) otherwise answers {statusCode: 200, body: 'LAMBDA-200'} — one function used as the authorizer, a separate one as the integration, so each is countable.
2. CreateRestApi (REGIONAL); create resource /{proxy+} with method ANY authorizationType=CUSTOM bound to a TOKEN authorizer (CreateAuthorizer, identitySource=method.request.header.Authorization, authorizerResultTtlInSeconds=0) and an AWS_PROXY integration onto the integration function; lambda AddPermission for apigateway.amazonaws.com on both functions; CreateDeployment to stage 'live'.
3. Wait for the stage to answer stably: GET <stage>/ok with Authorization: allow -> 200 and with Authorization: deny -> 403, then align to a minute boundary (t0).
4. Send 5 GET <stage>/ok with Authorization: allow (each must be 200 'LAMBDA-200') and 7 with Authorization: deny (each must be 403, body {"Message":"User is not authorized to access this resource with an explicit deny in an identity-based policy"}, x-amzn-ErrorType: AccessDeniedException).
5. THE GRADED READ: cloudwatch.GetMetricStatistics(Namespace='AWS/ApiGateway', MetricName='Count', Dimensions=[{'Name':'ApiName','Value':<api name>},{'Name':'Stage','Value':'live'}], StartTime=t0, EndTime=<now+3min>, Period=60, Statistics=['Sum']).
   EXPECTED (AWS): datapoints whose Sum totals 12.
   OBSERVED: Datapoints == [] (sum 0 over 0 points), unchanged for 240 s.
6. Repeat the read for MetricName='4XXError' (EXPECTED Sum 7), '5XXError' (EXPECTED at least one datapoint, Sum 0 — the series must exist and read zero), and 'Latency' with Statistics=['SampleCount'] (EXPECTED SampleCount 12). OBSERVED: all empty.
7. Read AWS/Lambda Invocations Sum for each function: EXPECTED 12 for the authorizer and 5 for the integration (the 7 denied requests must never reach the integration).
8. Re-replay apigateway__cloudwatch__lambda.metrics-elbv2-apigw-rest-api-lambda-authorizer-denied-403-counts-4xx-not-5xx.
