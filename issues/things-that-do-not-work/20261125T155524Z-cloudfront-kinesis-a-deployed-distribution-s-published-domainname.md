Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing
Issue-Key: cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing|_served#31|PublishedUrlRejected|url, route_headers = ctx.endpoint_route(published)
Issue-Key: cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis|run#41|PublishedUrlRejected|edge, route_headers = ctx.endpoint_route(domain)
Issue-Key: cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing|run#40|PublishedUrlRejected|base, route_headers = ctx.endpoint_route(dist["DomainName"])
Issue-Commit: e9279a32366be341735568d509d6722efe13b076

# cloudfront__kinesis: a Deployed distribution's published DomainName is a <id>.cloudfront.net host the emulator does not serve, so all three real-time-log-to-Kinesis checks abort before any viewer request is made

> _Note: this issue was identified against the code base at commit `e9279a32366be341735568d509d6722efe13b076`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistribution

## Description
Cause 1 of 1: the distribution DomainName the emulator publishes (and echoes on GetDistribution / CreateDistribution) is an address the emulator does not serve, so every viewer request a client builds from that published value is unroutable. All three real-time-log checks now fail at the moment they turn the published DomainName into a viewer URL — strictly earlier than the delivery step, so no real-time-log assertion is reached at all. Failing checks: cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis, cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing, cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing.

AWS: Smithy (com.amazonaws.cloudfront, 2020-05-31) models Distribution.DomainName as a required string, documented as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net". The contract that matters to a client is not the literal text but that the published DomainName is the address at which that distribution's edge actually answers viewer traffic: AWS returns a name that resolves and serves, so `GET https://<Distribution.DomainName>/<key>` reaches the distribution and is answered by it (200 with the origin object for an object the origin serves). Real AWS behaviour here is documented behaviour only (the model says nothing about reachability); the recordings show the viewer GET against the published DomainName succeeding, which is the precondition every real-time-logs assertion is measured after.

Emulator: DomainName is published in the <distribution-id>.cloudfront.net form, i.e. a public-DNS CloudFront host rather than an address this emulator listens on. The harness refuses it as soon as it is used as an endpoint: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` (raised in context.py _published_target, from endpoint_route). The same refusal appears for the bare domain and for a full object URL `https://<id>.cloudfront.net/<probe-key>`. The host, id and port are per-run values; the defect is that the announced host is not one the emulator answers on.

Why: a client that reads Distribution.DomainName and sends viewer traffic to it — which is the only address CloudFront gives it — can never reach the distribution. Every edge-dependent behaviour is unobservable through the documented path: no real-time log line can be produced because no viewer request can be served, and the authorization-negative cases (a delivery role without kinesis:PutRecord/PutRecords, or with an explicit Deny) cannot even establish their control side. CDN consumers, health probes and cache-behaviour tests all break at the first hop.

Fix: the CloudFront distribution's domain-name assignment and the edge listener binding — the component that mints Distribution.DomainName on CreateDistribution and echoes it on GetDistribution/ListDistributions must publish an address the emulator actually serves viewer traffic on (its own host/port, or a per-distribution listener), and the edge request path must accept and route viewer requests arriving at that announced address to the distribution's origin. Once that holds, the real-time-log delivery component (behavior RealtimeLogConfigArn -> project the request onto the config's Fields in CloudFront's canonical order -> PutRecord(s) to KinesisStreamConfig.StreamARN under the config's RoleARN, dropping only the delivery when that role lacks kinesis:PutRecord/PutRecords) becomes reachable.

Related but NOT covering this: 20261125T054637Z-cloudfront-kinesis-the-cloudfront-real-time-log-delivery.md carries these same three check ids for the earlier symptom (each check exhausting its 60 s budget inside the create-config -> Deployed -> edge-request -> GetRecords sequence); the checks now fail at a different and earlier point, on the published-endpoint refusal above, which is an unreachable announced edge address rather than a missing delivery step. Several other open issues report the same announced-address shape for other service groupings (e.g. 20261125T052443Z-apigateway-cloudfront-s3-the-published-distribution-domainname-is.md, 20261125T052443Z-cloudfront-s3-the-distribution-s-published-domainname-resolves.md, 20261125T150300Z-cloudfront-ec2-elbv2-a-vpc-origin-distribution-publishes.md) but none carries the cloudfront__kinesis real-time-log checks.

Traces: cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis; cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing; cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing.

## Evidence
Suite report, service heading '### cloudfront__kinesis' (0/3 passed, 3 failed):

[mismatch] cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing
  File "context.py", line 616, in endpoint_route -> _published_target (line 632)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/obj-c19915a24d56.txt (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:43507/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

[mismatch] cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34019/<resource path>, ...

[mismatch] cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; ... http(s)://127.0.0.1:38447/<resource path>, ...

Expected (AWS): GetDistribution returns Distribution.DomainName (required string, documented form d111111abcdef8.cloudfront.net) naming the address at which that distribution answers viewer traffic; `GET https://<DomainName>/<probe-key>` on a Deployed distribution is answered by the edge (200, the origin object's bytes), after which the configured real-time log line is delivered to the Kinesis stream. The specific id, host, probe key and port in the lines above are per-run values filling that template, not requirements.

Actual (emulator): the published DomainName is a <distribution-id>.cloudfront.net host that is not one of the emulator's own hosts; using it as the viewer endpoint is refused outright, so no viewer request is ever issued and no real-time log record is ever produced.

## Steps to Reproduce
1. kinesis.CreateStream(StreamName=<s>, ShardCount=1); poll DescribeStreamSummary until StreamStatus == ACTIVE; note StreamARN.
2. iam.CreateRole trusted by cloudfront.amazonaws.com; iam.PutRolePolicy allowing kinesis:DescribeStream, kinesis:DescribeStreamSummary, kinesis:PutRecord, kinesis:PutRecords on that StreamARN.
3. cloudfront.CreateRealtimeLogConfig(Name=<n>, SamplingRate=100, Fields=[timestamp, cs-uri-stem, sc-status, x-edge-request-id], EndPoints=[{StreamType: Kinesis, KinesisStreamConfig: {RoleARN: <role>, StreamARN: <stream>}}]) -> 201.
4. Create an S3 bucket, put a small object <probe-key>, create an OriginAccessControl, and cloudfront.CreateDistribution with that S3 origin, managed CachingDisabled CachePolicyId 4135ea2d-6df8-44a3-9df3-4b5a84be39ad and DefaultCacheBehavior.RealtimeLogConfigArn = the config ARN. Poll GetDistribution until Status == Deployed.
5. Read Distribution.DomainName from that GetDistribution response and use it, unmodified, as the viewer endpoint: GET https://<DomainName>/<probe-key>.
   Expected: the request reaches the distribution and is answered 200 with the object's bytes.
   Observed: the published DomainName is <distribution-id>.cloudfront.net, a host the emulator does not serve, so the endpoint is rejected before any request is sent (PublishedUrlRejected: "its host is not one of the emulator's own hosts").
6. Because step 5 cannot be performed, the follow-on assertion — GetShardIterator(TRIM_HORIZON) + GetRecords on the stream yielding exactly one tab-delimited line with the configured fields, cs-uri-stem '/<probe-key>' and sc-status '200' — is never reached. Same for the two authorization-negative variants (role limited to kinesis:DescribeStream* , or with an explicit Deny on PutRecord/PutRecords, alongside a correctly-permissioned control stream).
