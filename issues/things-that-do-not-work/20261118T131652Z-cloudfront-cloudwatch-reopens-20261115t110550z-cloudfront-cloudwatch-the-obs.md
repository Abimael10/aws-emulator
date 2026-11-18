Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes|run#16|Timeout|time.sleep(max(1.0, min(_METRIC_INTERVAL_S, float(until_next_round))))
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# cloudfront__cloudwatch: REOPENS 20261115T110550Z-cloudfront-cloudwatch-the-obs-cloudfront-1-additional-metrics--r2.md — the additional-metrics reconciliation still reaches no verdict; no AWS/CloudFront datapoint is ever observed for served edge traffic (600 s budget exhausted)

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMonitoringSubscription

## Description
REOPENS 20261115T110550Z-cloudfront-cloudwatch-the-obs-cloudfront-1-additional-metrics--r2.md (status: fixed): its fix left this check in exactly the same state — it still reaches no verdict and is killed at the suite's 600 s budget with no AWS/CloudFront datapoint observed. The behavior is re-reported here rather than described as a new defect. The replay marked this check neither OWNED nor ALREADY FILED (a fixed issue never owns a failure), so it survives to be filed.

One root cause (Cause 1 of 1). Failing check: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes.

Cause 1 of 1: a distribution with an ENABLED monitoring subscription publishes no AWS/CloudFront additional metric series for traffic its edge serves, so the check that drives edge requests and then reconciles the published datapoints against the outcomes it recorded from the responses never evaluates a single comparison.

AWS:      recorded behavior only for the metric values — the Smithy model carries the control-plane shapes (CreateMonitoringSubscription / MonitoringSubscription.RealtimeMetricsSubscriptionConfig.RealtimeMetricsSubscriptionStatus = Enabled|Disabled), not the metric catalogue. Enabling the subscription turns on CloudFront's additional metrics for that distribution; once viewer requests are served at the distribution's edge domain, the AWS/CloudFront namespace carries Requests (Sum), CacheHitRate (Average), OriginLatency (p90 extended statistic) and the 4xx/403/5xx error-rate series under the dimension pair DistributionId=<distribution id> + Region='Global', and GetMetricStatistics returns datapoints for them inside the check's 30 s polling cadence. The requirement is that relation between served edge outcomes and published datapoints (the check derives every expected value from the X-Cache headers and status codes the edge actually answered, within its own documented slack of ±1 request and 0.1 percentage points); the counts, ids and the particular minute of one run are not requirements.
Emulator: the check produces no assertion result at all — the replay detail is 'Error: Check timed out after 600.0s', verdict mismatch. Its awaited effects are a GetDistribution poll for Status=='Deployed' (1200 s), a bounded GET poll for a 200 from the distribution's DomainName (300 s), and a GetMetricStatistics poll for a first AWS/CloudFront CacheHitRate datapoint on the subscribed distribution (900 s, every 30 s). The run budget expires inside those waits; no AWS/CloudFront datapoint is ever seen. Its per-request HTTP timeout is 15 s and it marks a never-serving edge with its own DATAPLANE_UNIMPLEMENTED marker precisely so this reads as a fidelity failure rather than a runner/environment error.
Why:      a client that enables a monitoring subscription and builds dashboards or alarms on CacheHitRate / OriginLatency / *ErrorRate sees permanently empty series; alarms over them stay in INSUFFICIENT_DATA instead of tracking the edge, so cache-tuning, paging and autoscaling decisions driven by those metrics never fire, and there is no way to verify the opt-in had any effect. Every assertion past the barrier (datapoints agreeing with the recorded hit/miss/404 outcomes) is unverified.
Fix:      the CloudFront edge data plane's per-request outcome recording plus the AWS/CloudFront metric-publication path that CreateMonitoringSubscription enables — the component that turns served edge responses into CloudWatch datapoints under DistributionId + Region='Global' — and the distribution lifecycle/edge host that must first reach Deployed and answer at the DomainName it publishes. Not the check.
Related, NOT covering: open issue 20261115T092147Z-cloudfront-cloudwatch-the-monitoring-subscription-metric-gating-check.md carries the SIBLING check (obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription, the gating/negative half) and not this one; open issues 20261110T090020Z-cloudfront-cloudwatch-a-served-edge-produces-no-aws.md and 20261110T162302Z-cloudfront-cloudwatch-cloudfront-additional-monitoring-subscription-metrics-check-never-completes.md are the same area. None carry this check's replay key.
Traces:   cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 531/702 passed (75.6%); bootstrap_signal false. Under '### cloudfront__cloudwatch': 0/2 (0%), 2 failed (the sibling gating check is owned by 20261115T092147Z-...; this one is not).

  [mismatch] cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes
    Error: Check timed out after 600.0s

Actual: no assertion result is produced. The check's internal barriers each exceed the run budget on this emulator — _wait_distribution_deployed (GetDistribution until Status=='Deployed', 1200 s), _fetch_ok (GET the distribution's DomainName until 200, 300 s) and _await_additional_metrics_live (GetMetricStatistics in AWS/CloudFront for CacheHitRate with Dimensions DistributionId=<id>, Region='Global' until >=1 datapoint, 900 s at 30 s intervals) — and the run ends with no AWS/CloudFront datapoint ever observed (the check never even reaches its own 900 s give-up point).

Expected (AWS), per the check's contract: after CreateMonitoringSubscription(RealtimeMetricsSubscriptionStatus='Enabled') on a Deployed distribution and the measured burst it issues inside one 60 s period (1 + 3 repeats on a key the origin holds, 2 on a key it does not), GetMetricStatistics for that minute returns exactly one datapoint for each of Requests (Sum), CacheHitRate, 4xxErrorRate, 403ErrorRate, 5xxErrorRate and OriginLatency (p90) under DistributionId=<id>, Region='Global', and each value reproduces the outcomes the check recorded from the edge responses (X-Cache hit/miss and status codes) within ±1 request and 0.1 percentage points.

This is not a runner fault: the check's HTTP calls carry a 15 s per-request timeout, and its DATAPLANE_UNIMPLEMENTED marker exists to classify a never-answering edge as a fidelity failure. The previously FIXED issue 20261115T110550Z-...-r2.md recorded the identical symptom for the identical check id.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout; replay the behavioral suite simparity-behavioral-aws-v1.
2. Create an S3 bucket holding one object at key 'present-1.txt' and leave 'absent-never-written.txt' unwritten; create an origin access control (type s3, sigv4/always) and grant the distribution s3:GetObject only (no s3:ListBucket) on the bucket.
3. CreateDistribution over that bucket with the managed cache policy Managed-CachingOptimized (658327ea-f89d-4fab-a63d-7e88639e58f6) and poll GetDistribution until Status == 'Deployed'.
4. CreateMonitoringSubscription(DistributionId=<id>, MonitoringSubscription={'RealtimeMetricsSubscriptionConfig': {'RealtimeMetricsSubscriptionStatus': 'Enabled'}}) and confirm GetMonitoringSubscription reads back 'Enabled'.
5. GET the distribution's published DomainName for 'present-1.txt' once plus 3 repeats and for 'absent-never-written.txt' twice, inside one 60 s wall-clock period, recording each status and X-Cache value.
6. Poll GetMetricStatistics in namespace AWS/CloudFront with Dimensions DistributionId=<id>, Region='Global' for Requests (Sum), CacheHitRate (Average) and OriginLatency (ExtendedStatistics p90) over that minute.
7. Observed: no datapoint appears for the entire 600 s budget and the check is killed with no verdict. Expected: one datapoint per metric for that minute, reproducing the outcomes recorded in step 5.
8. Re-replay cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes to confirm the fix.
