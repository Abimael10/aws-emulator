Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes|run#16|PublishedUrlRejected|base, route = ctx.endpoint_route(dist["DomainName"])
Issue-Key: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription|run#33|PublishedUrlRejected|routes = {side: ctx.endpoint_route(d["DomainName"]) for side, d in dists.items()}
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudfront__cloudwatch: the distribution's published DomainName is an unserved <id>.cloudfront.net host again, so no edge request is ever made and no AWS/CloudFront additional metric can be reconciled

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
REOPENS 20261109T181319Z-cloudfront-cloudwatch-the-distribution-s-published-domainname-is.md: its fix left the call refused. That issue reported the published DomainName as an unserved <id>.cloudfront.net host blocking the additional-metrics checks, and the member is again exactly that, so this is the same defect, not a new one.

Cause 1 of 1: the distribution these checks create publishes Distribution.DomainName as a real-AWS <id>.cloudfront.net host that the emulator does not answer on, so the edge requests whose outcomes the metrics must reflect are never made.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", and that host is the address the edge serves on. Recorded behavior only for the rest: viewer requests addressed to it are answered by the edge, and with a monitoring subscription enabled the AWS/CloudFront namespace then carries the additional per-distribution series (CacheHitRate, OriginLatency, 4xx/5xx error-rate) matching those outcomes, while without a monitoring subscription only the always-on series appear.

Emulator: the published host is not one of the emulator's own hosts, so the harness refuses to route the warm-up/probe request rather than leaving the emulator: 'endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' (simparity.published.PublishedUrlRejected). Both checks die at that first request, so the metric assertions are never reached.

Why: a client that enables a CloudFront monitoring subscription and then drives traffic to measure it can neither drive the traffic nor observe the metric boundary; the opt-in/opt-out difference in the AWS/CloudFront namespace is unobservable because the address published for the edge is not the emulator's.

Fix: the CloudFront distribution state/serializer that mints the published domain name plus the edge listener it must name; the metric emission path depends on edge requests actually being served on the published address.

Traces: cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes, cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription.

## Evidence
Expected: a viewer GET to https://<published DomainName>/<key> is served by the emulator's edge, and the AWS/CloudFront additional metric series then reflect those outcomes (present with a monitoring subscription, withheld without one).

Actual (both checks):
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:44541/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
    (obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net ... needs http(s)://127.0.0.1:40511/<resource path> ...
    (obs-cloudfront-1-additional-metrics-withheld-without-monitoring-subscription)

Host label and port are per-run values; the divergence is that the published host is real AWS's rather than an address the emulator is reached on.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront__cloudwatch.obs-cloudfront-1-additional-metrics-match-recorded-edge-outcomes: create a distribution, enable a monitoring subscription, wait for Deployed, read Distribution.DomainName.
3. Observe DomainName is <id>.cloudfront.net; the edge warm-up request is refused by the harness (PublishedUrlRejected), so no metric datapoint can be produced or asserted.
