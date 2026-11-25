Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__logs.obs-cloudfront-3-vended-access-logs-match-requests-and-stop-after-delete-delivery cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group
Issue-Key: cloudfront__logs.obs-cloudfront-3-vended-access-logs-match-requests-and-stop-after-delete-delivery|run#30|PublishedUrlRejected|base, route_headers = ctx.endpoint_route(domain)
Issue-Key: cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group|run#38|PublishedUrlRejected|edge, route_headers = ctx.endpoint_route(dist["DomainName"])
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudfront__logs: standard logging v2 and vended access logs cannot be observed — the published DomainName is an unserved <id>.cloudfront.net host, so no edge request is ever logged

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
REOPENS 20261111T111451Z-cloudfront-logs-a-cloudfront-access-logs-delivery-with.md: its fix left the call refused. That issue reported that the edge never answers the request to be logged, and the edge is still unaddressable — now because the distribution publishes an <id>.cloudfront.net DomainName the emulator does not serve — so this is the same defect rather than a new one.

Cause 1 of 1: the distribution publishes Distribution.DomainName as a real-AWS <id>.cloudfront.net host the emulator does not answer on, so the viewer requests that standard logging v2 must record are never made.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", the address the edge serves on. Recorded behavior only for the rest: viewer requests addressed to it are answered by the edge, and an enabled CloudWatch Logs delivery for the distribution then carries one access-log record per request (with the request's fields), stopping once the delivery is deleted.

Emulator: the published host is not one of the emulator's own hosts, so the harness refuses to route the request: 'endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' (simparity.published.PublishedUrlRejected). Both checks stop at that point, before any log-group assertion.

Why: a client wiring CloudFront access logs to a log group can neither generate the traffic to be logged nor confirm that logging starts and stops with the delivery, because the published edge address is not the emulator's.

Fix: the CloudFront distribution state/serializer that mints the published domain name plus the edge listener it must name; the access-log emission path depends on viewer requests actually being served on the published address.

Traces: cloudfront__logs.obs-cloudfront-3-vended-access-logs-match-requests-and-stop-after-delete-delivery, cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group.

## Evidence
Expected: a viewer GET to https://<published DomainName>/<key> is served by the emulator's edge and produces one access-log record in the configured destination, and no further records after the delivery is deleted.

Actual (both checks):
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34129/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
    (obs-cloudfront-3-vended-access-logs-match-requests-and-stop-after-delete-delivery)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net ... needs http(s)://127.0.0.1:44207/<resource path> ...
    (xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group)

Host label and port are per-run values; the divergence is the published host being real AWS's instead of an address the emulator is reached on.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront__logs.xsvc-cloudfront-3-standard-logging-v2-access-log-reaches-log-group: create a distribution, configure a standard-logging-v2 delivery to a log group, wait for Deployed, read Distribution.DomainName.
3. Observe DomainName is <id>.cloudfront.net; the viewer GET to be logged is refused by the harness (PublishedUrlRejected) because that host is not served by the emulator.
