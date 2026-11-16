Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes|run#16|Timeout|time.sleep(max(1.0, min(_METRIC_INTERVAL_S, float(until_next_round))))
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# cloudfront__cloudwatch: a monitoring-subscribed distribution still publishes no AWS/CloudFront additional metric series (CacheHitRate/OriginLatency/403ErrorRate), so the obs-cloudfront-1 reconciliation never reaches a verdict (600 s timeout)

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMonitoringSubscription

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: a CloudFront distribution whose monitoring subscription is Enabled publishes no AWS/CloudFront ADDITIONAL metric series for the viewer traffic its edge serves, so the check that drives a measured minute of edge traffic and then reconciles the published datapoints against the outcomes it recorded from the edge responses never reaches a single comparison — it is killed at the suite's 600 s budget while still polling for a first datapoint. Failing check: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes.

AWS: recorded behavior only for the metric catalogue — the Smithy model carries only the control-plane shapes (CreateMonitoringSubscription / GetMonitoringSubscription, MonitoringSubscription.RealtimeMetricsSubscriptionConfig.RealtimeMetricsSubscriptionStatus = Enabled|Disabled; cloudfront is a global resource and publishes to CloudWatch in us-east-1). Enabling the subscription turns on CloudFront's additional metrics for that distribution: from roughly four minutes after CreateMonitoringSubscription, served viewer traffic yields, for each 60 s period, datapoints in namespace AWS/CloudFront under the dimension pair DistributionId=<distribution id>, Region='Global' for CacheHitRate (Average, Unit 'Percent'), 4xxErrorRate / 403ErrorRate / 5xxErrorRate (Average, Unit 'Percent') and OriginLatency (ExtendedStatistics p90, Unit 'Milliseconds'), alongside the always-published default series Requests (Sum, Unit 'None'). The REQUIREMENT is the relation between the outcomes the edge served in that minute and those datapoints: Requests Sum equals the number of requests served in the period; CacheHitRate equals 100 * (X-Cache 'Hit from cloudfront' responses) / cacheable requests; the 4xx/403/5xx rates equal the share of that period's requests answered with those statuses; OriginLatency p90 is a positive millisecond latency. The counts, ids, bucket names and the particular minute of any one run are not part of the requirement.

Emulator: the check evaluates no comparison at all. Replay detail is exactly `Error: Check timed out after 600.0s` (verdict: mismatch). The check's own barriers are a GetDistribution poll for Status=='Deployed' (1200 s), a 200 from the distribution DomainName (300 s), then `_await_additional_metrics_live` — GetMetricStatistics polled every 30 s for ONE CacheHitRate datapoint under DistributionId/Region=Global (900 s) — and only then the measured rounds, which require Requests, CacheHitRate, 4xxErrorRate, 403ErrorRate, 5xxErrorRate and OriginLatency(p90) all present for the SAME minute. The run is cut off inside the awaited effect; no additional-metric datapoint is observed. Hypothesis: the default series now publish (this heading's sibling check, obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription, passes in this run, and that check grades the unsubscribed control's Requests/4xxErrorRate), so what is missing is specifically the subscription-gated set — CacheHitRate, OriginLatency (and its p90 extended statistic) and 403ErrorRate — never being published for the subscribed distribution. All HTTP calls in the check carry a 15 s per-request timeout and a never-serving edge is marked DATAPLANE_UNIMPLEMENTED, so the exhausted budget is spent waiting on the emulator, not in the runner.

Why: a client that opts in with CreateMonitoringSubscription sees permanently empty CacheHitRate / OriginLatency / per-status error-rate series. Dashboards read blank, alarms over them never leave INSUFFICIENT_DATA (so cache-tuning, paging and scaling decisions driven by them never fire), and the opt-in is indistinguishable from leaving it off.

Fix: the CloudFront edge data plane's per-request outcome recording plus the AWS/CloudFront metric-publication path gated by the monitoring subscription — the component that turns served edge responses into CloudWatch datapoints under DistributionId + Region=Global in the cloudfront crate, and the metric store that must answer GetMetricStatistics for them (including the p90 extended statistic and the documented Units) in the cloudwatch crate. Not the check.

Context, not coverage: the suite marked this check neither OWNED nor ALREADY FILED. The fixed issue 20261115T110550Z-cloudfront-cloudwatch-the-obs-cloudfront-1-additional-metrics--r2.md (and before it 20261111T170129Z-cloudfront-cloudwatch-the-obs-cloudfront-1-additional-metrics.md) claimed this same check; their fixes left it still unable to observe a datapoint, so this is re-reported rather than described as a brand-new defect. Open issues in the same area that do NOT carry this check's replay key: 20261110T090020Z-cloudfront-cloudwatch-a-served-edge-produces-no-aws.md, 20261110T162302Z-cloudfront-cloudwatch-cloudfront-additional-monitoring-subscription-metrics-check-never-completes.md, 20261115T092147Z-cloudfront-cloudwatch-the-monitoring-subscription-metric-gating-check.md, 20261109T181319Z-cloudfront-cloudwatch-the-distribution-s-published-domainname-is.md.

Traces: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes.

## Evidence
Suite: simparity-behavioral-aws-v1, 496/632 passed (78.5%). Under `### cloudfront__cloudwatch`: 1/2 (50%), 1 failed.

  [mismatch] cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes
    Error: Check timed out after 600.0s

Expected (AWS): after CreateMonitoringSubscription(RealtimeMetricsSubscriptionStatus='Enabled') on a Deployed distribution and one measured minute of viewer traffic (1 first request + 3 repeats on a key the origin holds, 2 requests on a key it does not), GetMetricStatistics in namespace AWS/CloudFront with Dimensions DistributionId=<id>, Region='Global' returns, for that 60 s period: Requests Sum == the number of requests served (Unit 'None'); CacheHitRate Average == 100 * hits / cacheable requests (Unit 'Percent'); 4xxErrorRate / 403ErrorRate / 5xxErrorRate Average == the share of that minute's requests answered with those statuses (Unit 'Percent'); OriginLatency ExtendedStatistics p90 a positive value in Milliseconds.

Actual (emulator): no AWS/CloudFront additional-metric datapoint is ever returned. The check polls at a 30 s cadence and is killed at the harness's 600 s budget — before even reaching its own 900 s metric give-up point — so not one of the comparisons above is evaluated. Its sibling check in the same heading (the unsubscribed control, which grades only the default Requests/4xxErrorRate series) passes in this same run.

## Steps to Reproduce
1. Create an S3 bucket, put an object at key `present-0.txt` plus a `warmup.txt`, and leave `absent-never-written.txt` unwritten.
2. CreateOriginAccessControl (OriginAccessControlOriginType 's3', SigningBehavior 'always', SigningProtocol 'sigv4').
3. CreateDistributionWithTags over that bucket origin with the OAC attached and the managed cache policy Managed-CachingOptimized (658327ea-f89d-4fab-a63d-7e88639e58f6); grant the distribution ARN s3:GetObject (and NOT s3:ListBucket) in the bucket policy.
4. CreateMonitoringSubscription(DistributionId=<id>, MonitoringSubscription={'RealtimeMetricsSubscriptionConfig': {'RealtimeMetricsSubscriptionStatus': 'Enabled'}}) and confirm GetMonitoringSubscription reads back 'Enabled'. Wait for Status == 'Deployed'.
5. GET `warmup.txt` through the distribution's published DomainName until it answers 200, then in one fresh clock minute GET `present-0.txt` once plus 3 repeats (recording each status and X-Cache) and `absent-never-written.txt` twice (expect the origin's 403).
6. Poll cloudwatch (us-east-1) GetMetricStatistics in namespace AWS/CloudFront, Dimensions=[{DistributionId=<id>},{Region='Global'}], Period 60 over that minute for Requests (Sum), CacheHitRate / 4xxErrorRate / 403ErrorRate / 5xxErrorRate (Average) and OriginLatency (ExtendedStatistics p90).
Observed: no datapoint ever appears for the additional series; the check is killed at 600 s with `Error: Check timed out after 600.0s`. Expected: datapoints that reproduce the outcomes recorded in step 5 with the Units above. Re-replay cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes to confirm the fix.
