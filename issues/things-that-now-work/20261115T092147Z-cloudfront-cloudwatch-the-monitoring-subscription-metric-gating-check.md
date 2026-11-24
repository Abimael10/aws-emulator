Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription|run#49|Timeout|time.sleep(max(1.0, min(_METRIC_INTERVAL_S, float(until_next_round))))
Issue-Commit: c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9

# cloudfront__cloudwatch: the monitoring-subscription metric gating check never reaches a verdict — a Deployed distribution's edge serves nothing and no AWS/CloudFront series is published (600 s timeout)

> _Note: this issue was identified against the code base at commit `c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMonitoringSubscription

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: the CloudFront observability path (edge data plane + AWS/CloudFront metric publication into CloudWatch) produces nothing, so the subscription-gated-metrics contract cannot be observed at all; the check exhausts its 600 s budget instead of reaching a verdict.

AWS: recorded behavior only for the metric gating (the Smithy model carries the control-plane shapes, not the metric catalogue). CloudFront's CreateMonitoringSubscription / GetMonitoringSubscription operations turn on "additional" (real-time) metrics for one distribution: GetMonitoringSubscription on a subscribed distribution returns MonitoringSubscription.RealtimeMetricsSubscriptionConfig.RealtimeMetricsSubscriptionStatus = `Enabled`, and on an unsubscribed distribution it is refused with Code `NoSuchMonitoringSubscription` and message `The monitoring subscription does not exist.` (fixed text; no variable parts). Independently of the subscription, every distribution that serves viewer traffic publishes its DEFAULT AWS/CloudFront metrics — including `Requests` (Sum) and `4xxErrorRate` (Average) — dimensioned DistributionId=<distribution id> and Region=`Global`, and those series are listed by ListMetrics. The additional metrics `CacheHitRate`, `OriginLatency` and `403ErrorRate` are published (and listed) only for a distribution that has a monitoring subscription, and only for traffic from roughly four minutes after CreateMonitoringSubscription. Dimension names/values and metric names are the requirement; the generated distribution ids, bucket names and the particular minute of a run are not.

Emulator: the check never completes — replay detail is `Error: Check timed out after 600.0s`, verdict mismatch. The check creates two identical OAC-protected S3-origin distributions, subscribes only side A, waits for both to reach Status=`Deployed` (GetDistribution poll, budget 1200 s), fetches the warm-up object through each distribution's published DomainName, then polls CloudWatch for a first `CacheHitRate` datapoint on the subscribed side (budget 900 s) before measuring one minute of traffic. Nothing in that sequence ever satisfies its barrier inside the run budget: no verdict is produced, and in particular no AWS/CloudFront datapoint (gated or default) is ever observed for either distribution.

Why: a client that relies on CloudFront observability gets no CloudWatch telemetry for its distributions at all — `Requests`/`4xxErrorRate` dashboards stay empty, alarms on AWS/CloudFront never leave INSUFFICIENT_DATA, and the monitoring-subscription feature is indistinguishable from the unsubscribed case (the negative half of the contract passes vacuously, so turning a subscription on buys nothing). Because the check cannot even reach its measured round, every assertion past the barrier (default metrics matching what the control served, and the gated metrics being absent AND unlisted for the unsubscribed control) is unverified.

Fix: the cloudfront distribution lifecycle/edge data plane and the AWS/CloudFront metric-emission path that feeds CloudWatch, plus the monitoring-subscription gate that decides which metric names are published and listed for a distribution. The distribution's published DomainName must be an address the emulator actually serves, a Deployed distribution must answer viewer requests through its S3 origin, serving must publish the default AWS/CloudFront series under DistributionId + Region=Global, and CacheHitRate/OriginLatency/403ErrorRate must be published and listed only while a monitoring subscription exists.

Traces: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription.

Related open issues that look similar but were NOT reported as owning this check by the replay, named here for cross-reference: 20261110T162302Z-cloudfront-cloudwatch-cloudfront-additional-monitoring-subscription-metrics-check-never-completes.md, 20261110T090020Z-cloudfront-cloudwatch-a-served-edge-produces-no-aws.md and 20261109T181319Z-cloudfront-cloudwatch-the-distribution-s-published-domainname-is.md.

## Evidence
Check: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription (verdict: mismatch).

Actual (emulator): `Error: Check timed out after 600.0s` — the check produced no assertion result. Its internal barriers are a GetDistribution poll for Status=='Deployed' (1200 s), a _fetch_ok poll for a 200 from the distribution's DomainName (300 s) and a GetMetricStatistics poll for one AWS/CloudFront CacheHitRate datapoint on the subscribed distribution (900 s); each alone exceeds the 600 s run budget, and the run ends with no AWS/CloudFront datapoint ever seen.

Expected (AWS), per the check's contract:
- GetMonitoringSubscription(subscribed distribution).MonitoringSubscription.RealtimeMetricsSubscriptionConfig.RealtimeMetricsSubscriptionStatus == 'Enabled'.
- GetMonitoringSubscription(unsubscribed distribution) refused with ('NoSuchMonitoringSubscription', 'The monitoring subscription does not exist.').
- Both distributions reach Status 'Deployed' and serve viewer requests on their published DomainName: the present key returns 200 with the origin body, a key absent from the OAC-granted bucket (s3:GetObject only, no s3:ListBucket) returns the origin's 403.
- For the measured minute, the UNSUBSCRIBED control publishes AWS/CloudFront Requests (Sum == the number of requests it served) and 4xxErrorRate (Average == the 4xx share of that record) under Dimensions DistributionId=<control id>, Region='Global'.
- For that same minute the control publishes NO datapoints for CacheHitRate, OriginLatency or 403ErrorRate, and ListMetrics returns [] for those three metric names on the control while it DOES list its Requests metric; the subscribed twin, given identical traffic, does publish them.

## Steps to Reproduce
1. Create an S3 bucket and put a warm-up object; create a CloudFront origin access control (type s3, sigv4/always).
2. Create two distributions over that bucket with the managed cache policy Managed-CachingOptimized (658327ea-f89d-4fab-a63d-7e88639e58f6), identical except for the next step, and grant both s3:GetObject (and NOT s3:ListBucket) on the bucket.
3. Call CreateMonitoringSubscription on distribution A only, with RealtimeMetricsSubscriptionConfig.RealtimeMetricsSubscriptionStatus='Enabled'; confirm GetMonitoringSubscription reads back 'Enabled' for A and is refused NoSuchMonitoringSubscription for B.
4. Wait for both distributions to report Status='Deployed', then GET the warm-up key and a never-written key through each distribution's published DomainName — expect 200 with the origin body and 403 respectively.
5. Poll GetMetricStatistics in namespace AWS/CloudFront for CacheHitRate with Dimensions DistributionId=<A>, Region=Global until a datapoint appears, then send one identical burst to each distribution inside a single 60 s period.
6. For that minute, read the control's Requests (Sum) and 4xxErrorRate (Average), and assert CacheHitRate/OriginLatency/403ErrorRate have no datapoints and are not returned by ListMetrics for the control.
7. Observed: the run exhausts its 600 s budget before any AWS/CloudFront datapoint is seen. Re-replay check cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription to confirm the fix.
