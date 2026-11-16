Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes|run#16|Timeout|time.sleep(max(1.0, min(_METRIC_INTERVAL_S, float(until_next_round))))
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# cloudfront__cloudwatch: the obs-cloudfront-1 additional-metrics reconciliation still never reaches a verdict — no AWS/CloudFront additional metric series is published for served edge traffic (check exhausts its 600 s budget)

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMonitoringSubscription

## Description
Cause 1 of 1: a distribution with an ENABLED monitoring subscription publishes no AWS/CloudFront additional metric series, so the check that drives edge traffic and then reconciles the metrics against the outcomes it actually recorded never reaches a verdict — it is killed at the suite's 600 s budget while still polling for a first datapoint.

Note for the fixer: the fixed issue 20261111T170129Z-cloudfront-cloudwatch-the-obs-cloudfront-1-additional-metrics.md claimed this same check; its fix left the check still unable to observe a datapoint, so the behavior is re-reported here rather than as a new defect. The suite marked this check neither OWNED nor ALREADY FILED.

AWS: recorded behavior only for the per-datapoint values — the Smithy model describes only the control-plane shapes (CreateMonitoringSubscription / MonitoringSubscription.RealtimeMetricsSubscriptionConfig.RealtimeMetricsSubscriptionStatus = Enabled|Disabled). Enabling the subscription turns on CloudFront's additional metrics for that distribution; after viewer requests are served at the distribution's edge domain, the AWS/CloudFront namespace carries those series (Requests, CacheHitRate, OriginLatency and the 4xx/403/503 error-rate series) under the DistributionId=<id> + Region=Global dimension pair, and GetMetricStatistics/GetMetricData return datapoints for them within the check's polling budget (it polls every 30 s). The recording this check was calibrated against served every request it issued — one plus 3 repeats on a key the origin holds, 2 on a key it does not — and the derived metric values followed those recorded outcomes within the check's documented slack (±1 request on Requests, 0.1 percentage points on rates). The requirement is that relation between served edge outcomes and published datapoints; the individual counts of one run are not.
Emulator: the check never evaluates any comparison. The suite reports `Error: Check timed out after 600.0s`: it is still waiting for an awaited effect (a Deployed distribution's edge answering, then a first AWS/CloudFront datapoint) when the budget expires. No AWS/CloudFront additional-metric datapoint is ever observed. The check marks a never-serving edge with DATAPLANE_UNIMPLEMENTED precisely so that this reads as a fidelity failure rather than a runner/environment error, and its HTTP calls carry a 15 s per-request timeout, so the exhausted budget is spent inside the awaited effect, not in the runner.
Why: a client that enables a monitoring subscription and then builds dashboards or alarms on CacheHitRate / OriginLatency / *ErrorRate sees permanently empty series. Alarms over them stay in INSUFFICIENT_DATA forever instead of tracking the edge, so paging, autoscaling and cache-tuning decisions driven by those metrics never fire, and there is no way to verify that the opt-in had any effect.
Fix: the CloudFront edge data plane's per-request outcome recording plus the AWS/CloudFront metric-publication path that CreateMonitoringSubscription enables (the component that turns served edge responses into CloudWatch datapoints under DistributionId/Region=Global) — not the check. Related open issues in the same area, named as context only because the suite marked this check unowned: 20261110T090020Z-cloudfront-cloudwatch-a-served-edge-produces-no-aws.md, 20261110T162302Z-cloudfront-cloudwatch-cloudfront-additional-monitoring-subscription-metrics-check-never-completes.md, 20261115T092147Z-cloudfront-cloudwatch-the-monitoring-subscription-metric-gating-check.md.
Traces: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes.

## Evidence
Suite: simparity-behavioral-aws-v1, 484/632 passed (76.6%). Under `### cloudfront__cloudwatch`: 1/2 (50%), 1 failed.

  [mismatch] cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes
    Error: Check timed out after 600.0s

Expected: after CreateMonitoringSubscription(RealtimeMetricsSubscriptionStatus=Enabled) on a Deployed distribution and the 6 viewer requests the check issues, AWS/CloudFront datapoints exist under DistributionId=<id>, Region=Global for Requests, CacheHitRate, OriginLatency and the error-rate series, and agree with the hit/miss/404 outcomes the check recorded from the edge responses within its documented slack.
Actual: the check polls GetMetricStatistics/GetMetricData every 30 s and observes no datapoint at all for the whole 600 s budget, so it is cut off before it evaluates a single comparison (its own metric deadline is 900 s, i.e. it never even reaches its own give-up point).

## Steps to Reproduce
1. Create an S3 (or other) origin holding one object at key `present-1.txt`, and leave `absent-never-written.txt` unwritten.
2. CreateDistribution over that origin using the managed cache policy Managed-CachingOptimized (658327ea-f89d-4fab-a63d-7e88639e58f6); wait for Status == Deployed.
3. cloudfront.create_monitoring_subscription(DistributionId=<id>, MonitoringSubscription={"RealtimeMetricsSubscriptionConfig": {"RealtimeMetricsSubscriptionStatus": "Enabled"}}).
4. GET the distribution's DomainName for `present-1.txt` once plus 3 repeats, and `absent-never-written.txt` twice, recording each status and X-Cache value.
5. Poll cloudwatch GetMetricStatistics / GetMetricData in namespace AWS/CloudFront for Requests, CacheHitRate and OriginLatency with Dimensions DistributionId=<id>, Region=Global.
Observed: no datapoints for the entire budget; the check is killed at 600 s. Expected: datapoints that reproduce the outcomes recorded in step 4.
