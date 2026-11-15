Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription|run#49|Timeout|time.sleep(max(1.0, min(_METRIC_INTERVAL_S, float(until_next_round))))
Issue-Commit: fa5a827558c7852c4bea0e628f9a7809957676ef

# cloudfront__cloudwatch: the additional-metrics opt-in boundary is still unobservable — the monitoring-subscription check exhausts its 600 s budget with no AWS/CloudFront datapoint

> _Note: this issue was identified against the code base at commit `fa5a827558c7852c4bea0e628f9a7809957676ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: a distribution publishes no AWS/CloudFront metric series at all, so the opt-in boundary between the default metrics and the subscription-gated 'additional' metrics cannot be observed and the check never reaches a verdict.

AWS:      recorded/documented behavior only — CloudFront metric emission is not a Smithy shape. A deployed distribution ALWAYS publishes its default series (Requests, BytesDownloaded, BytesUploaded, TotalErrorRate, 4xxErrorRate, 5xxErrorRate) in namespace AWS/CloudFront under the dimension pair {DistributionId=<the distribution id>, Region=Global}. The additional series (CacheHitRate, OriginLatency and the per-status-code error-rate series such as 403ErrorRate) exist ONLY while a monitoring subscription with RealtimeMetricsSubscriptionStatus=Enabled is in place on that distribution: without one those series must return no datapoint, with one they must appear for the same traffic. Both arms of the check use the same AWS-published managed cache policy (Managed-CachingOptimized, 658327ea-f89d-4fab-a63d-7e88639e58f6) and the same request pattern, so the monitoring subscription is the only difference between them.
Emulator: the check issues its requests through each distribution's domain and then polls GetMetricStatistics for the gated and the default series until the harness budget expires. The replay reports only `Error: Check timed out after 600.0s` — no AWS/CloudFront datapoint of any kind is ever read, in either arm. Hypothesis (neither the model nor the recording states it): the distribution's published DomainName is a host the emulator does not answer on and/or no metric is published from the edge path, so both the traffic and the series are absent; the replay output carries no further diagnosis than the expired budget.
Why:      a client cannot distinguish 'these additional metrics are not subscribed' from 'CloudFront publishes nothing'. Every dashboard, GetMetricStatistics/GetMetricData query and alarm over an emulated distribution reads empty, and an alarm on 5xxErrorRate or CacheHitRate stays in INSUFFICIENT_DATA forever instead of evaluating.
Fix:      the cloudfront edge/data-plane path must serve viewer requests for a Deployed distribution and publish the default AWS/CloudFront series under {DistributionId, Region=Global} into the cloudwatch metric store, adding the subscription-gated series only while CreateMonitoringSubscription has left RealtimeMetricsSubscriptionStatus=Enabled for that distribution; the metric-read side (cloudwatch GetMetricStatistics) then needs no change.
Not covered by an existing issue: the open issue 20261110T162302Z-cloudfront-cloudwatch-cloudfront-additional-monitoring-subscription-metrics-check-never-completes.md describes the same scenario but carries a different replay key (its key names the `_poll_until#39` timeout frame, while this replay reports a bare 600 s budget expiry with no frame), so the suite does not treat this failure as owned; the sibling-cause issues 20261110T090020Z-cloudfront-cloudwatch-a-served-edge-produces-no-aws.md and 20261109T181319Z-cloudfront-cloudwatch-the-distribution-s-published-domainname-is.md should be closed together with it.
Traces:   cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription.

## Evidence
Expected (AWS): with NO monitoring subscription, GetMetricStatistics over Namespace=AWS/CloudFront, Dimensions=[{Name=DistributionId,Value=<id>},{Name=Region,Value=Global}] returns datapoints for the default series (Requests summing to the number of viewer requests issued, BytesDownloaded, TotalErrorRate) and NO datapoint for CacheHitRate/OriginLatency/403ErrorRate; after CreateMonitoringSubscription with RealtimeMetricsSubscriptionStatus=Enabled, the same query returns the gated series too.
Actual (emulator): `Error: Check timed out after 600.0s` — in both arms the poll never read a single AWS/CloudFront datapoint, default or gated, so neither the presence nor the absence claim could be graded.

## Steps to Reproduce
1. CreateDistribution over an origin, attach the managed cache policy 658327ea-f89d-4fab-a63d-7e88639e58f6, and poll GetDistribution until Status == 'Deployed'.
2. Do NOT create a monitoring subscription. Issue a handful of requests to the distribution's DomainName: some hits, some first-time misses, and one for a key the origin does not hold (a 4xx).
3. Poll cloudwatch GetMetricStatistics for Namespace=AWS/CloudFront, Dimensions=[{DistributionId},{Region=Global}], MetricName in {Requests, CacheHitRate, OriginLatency, 403ErrorRate}.
   Expected: Requests carries the issued request count; CacheHitRate/OriginLatency/403ErrorRate carry nothing.
   Actual: nothing is returned for any metric, and the check is killed at its 600 s budget.
4. Repeat with CreateMonitoringSubscription(RealtimeMetricsSubscriptionStatus=Enabled) before step 2 — the gated series are still absent.
