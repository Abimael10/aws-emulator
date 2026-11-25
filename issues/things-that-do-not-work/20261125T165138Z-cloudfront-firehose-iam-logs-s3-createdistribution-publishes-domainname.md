Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id
Issue-Key: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id|run#54|PublishedUrlRejected|base, route_headers = ctx.endpoint_route(domain)
Issue-Commit: fbb1a06f1c20de0c01a698c058987963365385aa

# cloudfront__firehose__iam__logs__s3: CreateDistribution publishes DomainName as a <id>.cloudfront.net host the emulator does not serve, so the standard-logging-v2 scenario is refused before any viewer request

> _Note: this issue was identified against the code base at commit `fbb1a06f1c20de0c01a698c058987963365385aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One net-new root cause, grounded in one failing behavioral check. The replay reports this check owned by no open issue and blocked by no unimplemented stub, so it is filed here.

Cause 1 of 1: CreateDistribution answers with a Distribution whose DomainName is a real-AWS-shaped `<14-char-id>.cloudfront.net` host. That host is not an address this emulator listens on and resolves to nothing reachable from a client of the emulator, so the published value cannot be used to address the distribution. The standard-logging-v2 end-to-end scenario is cut off at the moment it tries to turn that published DomainName into a viewer request: the edge is never contacted, and the whole CloudFront -> CloudWatch Logs delivery -> S3/Firehose log-record chain the check exists to verify is unobservable. Failing check: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id.

AWS: Smithy (cloudfront 2020-05-31) models Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net". On AWS that name is a real, resolvable DNS host that terminates viewer traffic for the distribution: a client takes DomainName out of CreateDistribution/GetDistribution and issues HTTP(S) viewer requests straight to it, and each request is served by the edge and mints an X-Amz-Cf-Id that later appears as x-edge-request-id in the access log. The shape of the label is per-run and is NOT the requirement; the requirement is that the announced DomainName is an address at which the emulator actually serves that distribution's viewer traffic.

Emulator: the distribution is created and the check reads DomainName, but the announced value is a cloudfront.net host that the emulator does not serve. The harness refuses it outright rather than dialling an address outside the emulator, with:
  `simparity.published.PublishedUrlRejected: endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` (the `<id>`, host and port are per-run values; the fixed text is the rejection sentence itself). The failure arises in the harness's endpoint_route/_published_target path, i.e. it is the emulator's published address that is wrong, not a runner tooling or DNS defect.

Why: a client that does what AWS documents — take the distribution's DomainName and send viewer requests to it — cannot reach the distribution at all. Every edge-dependent behaviour downstream of it (cache Miss/Hit classification, OAC-protected origin fetches, the 403 on an absent key, and then the ACCESS_LOGS delivery to both the S3 destination and the Firehose delivery stream joined on x-edge-request-id) is unreachable rather than merely wrong, and the client gets a connection/DNS failure instead of a service answer or a diagnosis.

Fix: the CloudFront distribution create/read serializers and the component that mints a distribution's published domain (CreateDistribution's handler and the shared published-address/endpoint derivation it uses, plus the matching GetDistribution/GetDistributionConfig and ListDistributions readback), together with the edge listener registration that must accept viewer traffic for that announced address. DomainName must be published as an address the emulator answers on and routes to this distribution, and the same value must be echoed consistently by the read paths.

Traces: cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id. Related but NOT covering this check: the open issues 20261118T021609Z-cloudfront-firehose-iam-logs-s3-the-cloudfront-standard.md and 20261125T050522Z-cloudfront-firehose-iam-logs-s3-the-standard-logging.md name this same check and subject, but both are grounded in a 60 s harness timeout with no answer at all; this run fails fast and names the concrete cause (an unserved published DomainName), so it is filed as its own cause and should be closed together with them. The same root cause is filed for other service headings (e.g. 20261125T152853Z-cloudfront-s3-wafv2-a-distribution-s-domainname-is.md, 20261125T150300Z-cloudfront-ec2-elbv2-a-vpc-origin-distribution-publishes.md, 20261125T052443Z-apigateway-cloudfront-s3-the-published-distribution-domainname-is.md); none of them carries this check's replay key.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 297/307 passed (96.7%), 8 mismatched, 2 errored, 0 skipped, bootstrap_signal false. Per-service rate for this heading: cloudfront__firehose__iam__logs__s3 1/2 (50%), 1 failed. Nine of the ten failures in this run are reported OWNED by open issues and were skipped; this is the only unowned failure.

Failing check `cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id`, verdict mismatch. Detail:
  File "context.py", line 616, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
  File "context.py", line 632, in _published_target
    raise PublishedUrlRejected(f"{message}: {problem[1]}", problem[0], value_class(value))
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:44205/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

The rejected value came from the emulator's own CreateDistribution response: the check binds `dist_id, dist_arn, domain = dist["Id"], dist["ARN"], dist["DomainName"]` from `cf.create_distribution(...)["Distribution"]` and then calls `ctx.endpoint_route(domain)`. (`dgzni4pnd7h2s3` and port 44205 are per-run values, not requirements.)

Expected (AWS): Distribution.DomainName is a resolvable CloudFront domain name (modelled example `d111111abcdef8.cloudfront.net`) that serves this distribution's viewer traffic; the three viewer GETs in this scenario are answered 200/Miss, 200/Hit and 403/Error, each minting a distinct X-Amz-Cf-Id, and one ACCESS_LOGS record per request then lands in BOTH the S3 destination and the Firehose destination carrying exactly x-edge-request-id, cs-uri-stem, sc-status and x-edge-result-type, with the records for one request identical across the two destinations.

Actual (emulator): DomainName is announced as a `<id>.cloudfront.net` host the emulator does not serve, so no viewer request is ever issued and none of the delivery-side assertions is reached. For comparison, the delivery-wiring steps the check performs before this point (firehose CreateDeliveryStream, logs PutDeliverySource / PutDeliveryDestination / CreateDelivery) did answer — the scenario fails exactly at the published edge address.

## Steps to Reproduce
1. Create an S3 origin bucket, put an object in it, and create a CloudFront origin access control (CreateOriginAccessControl).
2. Call CreateDistribution with that bucket as an S3 origin (OriginAccessControlId set) and the managed CachingOptimized cache policy 658327ea-f89d-4fab-a63d-7e88639e58f6, Enabled=true.
3. Read Distribution.DomainName from the response (and from GetDistribution). Observe it is a `<id>.cloudfront.net` host rather than an address the emulator serves.
4. Attempt an HTTP(S) viewer GET for the object key against that DomainName. Expected: the request is served by the edge (200, x-edge-result-type Miss, an X-Amz-Cf-Id header). Actual: the published host is not one of the emulator's own hosts, so the address is unreachable/refused and no viewer response is obtained.
5. In us-east-1, wire logs PutDeliverySource (the distribution, logType ACCESS_LOGS), PutDeliveryDestination for an S3 bucket and for a Firehose DirectPut delivery stream, and CreateDelivery for each; then confirm one access-log record per viewer request lands in both destinations with exactly [x-edge-request-id, cs-uri-stem, sc-status, x-edge-result-type] and joins on x-edge-request-id.
6. Re-replay cloudfront__firehose__iam__logs__s3.standard-logging-v2-joins-s3-and-firehose-deliveries-on-edge-request-id and confirm it reaches a verdict.
