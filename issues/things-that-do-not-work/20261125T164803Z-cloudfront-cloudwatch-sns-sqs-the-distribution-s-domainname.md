Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__cloudwatch__sns__sqs.obs-cloudfront-2-4xx-error-rate-alarm-notifies-sqs-untouched-control-stays-ok
Issue-Key: cloudfront__cloudwatch__sns__sqs.obs-cloudfront-2-4xx-error-rate-alarm-notifies-sqs-untouched-control-stays-ok|run#18|PublishedUrlRejected|edge, route_headers = ctx.endpoint_route(subject["DomainName"])
Issue-Commit: 5e8364ffd1cc635503e41df160d30e11335e3874

# cloudfront__cloudwatch__sns__sqs: the distribution's DomainName is published as a <id>.cloudfront.net host the emulator does not serve, so the 4xxErrorRate alarm -> SNS -> SQS chain is never exercised

> _Note: this issue was identified against the code base at commit `5e8364ffd1cc635503e41df160d30e11335e3874`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistribution

## Description
One net-new root cause, grounded in the single failing check the suite lists under `### cloudfront__cloudwatch__sns__sqs` (1/2, 1 failed), which the replay marked neither ALREADY FILED nor OWNED. Related OPEN issue that does NOT carry this check's replay key and so does not cover it: 20261125T060021Z-cloudfront-cloudwatch-sns-sqs-the-distribution-s-announced.md, whose Issue-Key binds the failure at `_fetch_ok#18` (an AssertionError after a refused TCP connection to the announced address). This replay fails EARLIER and differently: the harness rejects the announced address itself, at `context.py:616 endpoint_route` with `simparity.published.PublishedUrlRejected`, because the published host is a public AWS host rather than one of the emulator's own. Filed fresh so the announcement defect is gated by its own replay key.

Cause 1 of 1: a Deployed CloudFront distribution publishes a DomainName that is not an address the emulator serves — a `<id>.cloudfront.net` public AWS host — so a viewer request can never be addressed to the distribution at all and the whole scenario behind it (viewer GET -> AWS/CloudFront 4xxErrorRate -> CloudWatch alarm -> SNS topic -> subscribed SQS queue, with an untouched control distribution staying OK) is unreachable. Failing check: cloudfront__cloudwatch__sns__sqs.obs-cloudfront-2-4xx-error-rate-alarm-notifies-sqs-untouched-control-stays-ok.
AWS:      Smithy: cloudfront GetDistribution returns Distribution.DomainName ("The domain name that corresponds to the distribution") and Distribution.Status; cloudwatch PutMetricAlarm over namespace AWS/CloudFront with Dimensions DistributionId + Region='Global' and AlarmActions naming an SNS topic ARN; sns Publish to a subscribed sqs queue. On AWS the host published as DomainName is the host viewers address, and a Deployed distribution answers viewer GETs there: 200 for a key the origin holds and 4xx for one it does not (with the managed CachingDisabled policy every request reaches the origin). Those outcomes are metered as AWS/CloudFront 4xxErrorRate in us-east-1 under DistributionId=<id> + Region='Global', which drives an alarm whose Threshold is below that percentage to StateValue 'ALARM', whose SNS alarm action publishes, and whose notification arrives on the subscribed queue. The requirement is the relation and the addressability of the published host — the particular id, host, port, burst size and minute of any one run are not.
Emulator: GetDistribution publishes `DomainName` as a public CloudFront host. The replay refuses it before any request is made: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. The emulator therefore announced an address other than the one it is reached on: nothing it serves answers on that authority, so no viewer request, no 4xxErrorRate datapoint, no alarm transition, no SNS publish and no SQS message occur, and the control-stays-OK half is equally unverified.
Why:      a client that creates a distribution and reads its DomainName cannot reach the content it just published — every viewer request leaves the emulator for a host it does not own (or fails outright) — and every operational control built on edge error rates fails closed: the 4xxErrorRate alarm never leaves INSUFFICIENT_DATA, so the SNS notification and anything consuming it from SQS (paging, rollback, cache purge) never runs during a real error burst.
Fix:      the CloudFront distribution's address publication — the DomainName GetDistribution/CreateDistribution report must be an authority the emulator's edge listener answers on — and, behind it, the per-request outcome metering that publishes AWS/CloudFront 4xxErrorRate under DistributionId + Region='Global' in us-east-1, which the CloudWatch alarm evaluator drives to ALARM and whose alarm action fans out through SNS to the subscribed queue.
Traces:   cloudfront__cloudwatch__sns__sqs.obs-cloudfront-2-4xx-error-rate-alarm-notifies-sqs-untouched-control-stays-ok.

## Evidence
Suite replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 574/632 passed (90.8%), bootstrap_signal false. Under `### cloudfront__cloudwatch__sns__sqs`: 1/2 (50%), 1 failed, not OWNED and not ALREADY FILED.

  [mismatch] cloudfront__cloudwatch__sns__sqs.obs-cloudfront-2-4xx-error-rate-alarm-notifies-sqs-untouched-control-stays-ok
    File "context.py", line 616, in endpoint_route
      return self._published_target(url, "endpoint_url"), {}
    File "context.py", line 632, in _published_target
      raise PublishedUrlRejected(f"{message}: {problem[1]}", problem[0], value_class(value))
    simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:35701/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

Expected (AWS): GetDistribution reports a DomainName that is an address the service answers viewer requests on; a GET of the present key through it is answered 200 with the origin object's bytes, a burst of 5 GETs for a never-written key is answered 4xx, AWS/CloudFront 4xxErrorRate for that minute under DistributionId=<id> + Region='Global' in us-east-1 is >= 83%, the Threshold-20.0 / Period-60 alarm over it reaches StateValue 'ALARM', its SNS alarm action publishes, and the subscribed SQS queue receives the notification while the untouched control distribution's twin alarm stays 'OK'.
Actual: the published DomainName is a `<id>.cloudfront.net` public AWS host, which is not an address this emulator serves, so the request is never issued. The distribution id, host, loopback port and path in the detail above are per-run values; the requirement is that the host published as DomainName is one the emulator serves the edge on.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from that checkout, and replay simparity-behavioral-aws-v1.
2. In us-east-1, create an SNS topic and an SQS queue subscribed to it, with a topic policy allowing cloudwatch to publish.
3. Create an S3 origin holding a present key, leave a second key unwritten, and CreateDistribution over it twice (subject + untouched control) with the managed CachingDisabled cache policy (4135ea2d-6df8-44a3-9df3-4b5a84be39ad); poll GetDistribution until Status == 'Deployed' and read each published DomainName.
4. Observe that DomainName is a `<id>.cloudfront.net` host, not an authority the emulator serves. Expected: an address the emulator answers viewer requests on.
5. PutMetricAlarm on each distribution over AWS/CloudFront 4xxErrorRate, Dimensions DistributionId=<id> + Region='Global', Period 60, Threshold 20.0, GreaterThanThreshold, AlarmActions=[<topic arn>].
6. GET the present key at the published DomainName (expected 200), then issue 5 GETs for the never-written key on the subject distribution only, poll DescribeAlarms for StateValue == 'ALARM' and ReceiveMessage on the queue for the alarm notification, and confirm the control alarm is still 'OK'.
7. Re-replay cloudfront__cloudwatch__sns__sqs.obs-cloudfront-2-4xx-error-rate-alarm-notifies-sqs-untouched-control-stays-ok to confirm the fix.
