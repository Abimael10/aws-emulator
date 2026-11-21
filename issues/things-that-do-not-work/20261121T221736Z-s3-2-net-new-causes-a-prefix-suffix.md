Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.notification-filter-non-matching-key-not-delivered s3.s3-ssec-and-endpoint-enforcement-website-redirect-location-answers-301-while-rest-serves-the-body
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.notification-filter-non-matching-key-not-delivered|_drain_records#9|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Key: s3.s3-ssec-and-endpoint-enforcement-website-redirect-location-answers-301-while-rest-serves-the-body|_website_base_url#18|CapabilityNotOffered|candidates = ctx.s3_website_urls(
Issue-Commit: 344bb9336e0011430569d037e3a195e35382835e

# s3: 2 net-new causes — a prefix+suffix-filtered bucket notification never delivers its matching key within the check's window, and the emulator still publishes no S3 website endpoint so WebsiteRedirectLocation serving cannot be reached

> _Note: this issue was identified against the code base at commit `344bb9336e0011430569d037e3a195e35382835e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Two net-new causes in the S3 eventing and website-endpoint surfaces. Neither is marked owned by the replay report.

Cause 1 of 2: a bucket notification configuration with BOTH a prefix and a suffix FilterRule never produces an observable delivery, so the filter contract cannot be graded.
AWS: recorded behavior only for the delivery timing; the modeled surface is PutBucketNotificationConfiguration with a QueueConfiguration carrying Events ['s3:ObjectCreated:*'] and Filter.Key.FilterRules [{Name: prefix}, {Name: suffix}]. On AWS the configuration emits one s3:TestEvent at install time, then every PutObject whose key matches BOTH rules (prefix `incoming/` and suffix `.csv`) produces exactly one s3:ObjectCreated record in the configured SQS queue whose s3.object.key is that key, while keys breaking either rule produce no record at all yet are still stored (suppression is a routing decision, not a write failure).
Emulator: the replay reports only `Check timed out after 60.0s`. The check's first assertion is the positive control — the matching key `incoming/report.csv` must appear in the queue drain that follows its PutObject — and no verdict is produced before the suite's budget expires, so neither the matching delivery nor the suppression of the five non-matching keys is observable. Hypothesis: a double-rule (prefix AND suffix) Filter.Key configuration never matches, so no record is ever enqueued for the matching key; sibling filter checks under the s3__sqs heading are owned by 20261117T142140Z-s3-sqs-the-over-100-notification-configuration-refusal.md, which does not carry this check's replay key and so does not cover it.
Why: a client that relies on S3 prefix+suffix filtering to fan only the keys it cares about into a queue receives nothing at all against the emulator, which is indistinguishable from a correctly suppressing filter — the positive half of the contract is silently absent, so an integration validated here will drop every event in production terms.
Fix: the bucket-notification matching and dispatch path (the notification configuration's Filter.Key rule evaluation and the SQS dispatch that follows a successful PutObject).
Traces: s3.notification-filter-non-matching-key-not-delivered.

Cause 2 of 2: the emulator publishes no S3 static-website endpoint, so the WebsiteRedirectLocation serving contract is unreachable.
AWS: recorded behavior only for the endpoint hostnames; AWS documents two website host shapes per region (`<bucket>.s3-website-<region>.amazonaws.com` and `<bucket>.s3-website.<region>.amazonaws.com`), served over plain HTTP. On AWS, after PutBucketWebsite, a GET or HEAD of an object stored with WebsiteRedirectLocation answers HTTP 301 with a Location header holding exactly the stored value (`/new.html` for a relative one, `https://example.com/landing` for an absolute one) and never the object's body, while the REST endpoint for the same key at the same instant answers 200 with the body plus the `x-amz-website-redirect-location` header and NO Location header.
Emulator: the run names no website-endpoint host at all, so the check aborts before any website request is made: `simparity.behavioral.context.CapabilityNotOffered: the emulator offers no S3 website endpoint (the run names no website endpoint template: s3_website_url=, SIMPARITY_S3_WEBSITE_URL)`. The REST-side legs that precede it (HeadObject reporting the stored WebsiteRedirectLocation, and the InvalidRedirectLocation/400 refusal for a location that is neither `/...` nor http(s)) are reached; everything from the first website GET onwards is not. Open issues 20261118T010000Z-s3-the-rest-endpoint-serves-no-plaintext-http-and-no-website-endpoint.md and 20261117T213222Z-s3-4-net-new-causes-no-static-website.md record the same missing endpoint for other checks but do not carry this check's replay key; fixed issue 20261113T120806Z-s3-the-emulator-publishes-no-s3-website-endpoint.md claimed this surface, and this check shows it still unserved.
Why: any client or deployment that serves a site from S3 — redirect objects, index/error documents, routing rules — has no endpoint to address on the emulator, and the redirect-versus-REST distinction (301 + Location on the website host, 200 + body + x-amz-website-redirect-location on REST) cannot be exercised at all.
Fix: the S3 front end's endpoint publication and website request path — announce a reachable website host per bucket/region and route it through the website handler (index/error document, WebsiteRedirectLocation 301) instead of the REST object handler.
Traces: s3.s3-ssec-and-endpoint-enforcement-website-redirect-location-answers-301-while-rest-serves-the-body.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

Cause 1:
[mismatch] s3.notification-filter-non-matching-key-not-delivered
  Error: Check timed out after 60.0s
Expected (AWS): after PutBucketNotificationConfiguration with Filter.Key.FilterRules [{prefix: 'incoming/'}, {suffix: '.csv'}], a PutObject of `incoming/report.csv` lands exactly one s3:ObjectCreated record in the queue whose s3.object.key is that key; the five non-matching keys land none while still being listable; a second matching key is delivered afterwards.
Actual (emulator): no verdict — the positive-control drain following the matching PutObject is still unsatisfied when the suite's budget expires, so no delivery is observable.

Cause 2:
[mismatch] s3.s3-ssec-and-endpoint-enforcement-website-redirect-location-answers-301-while-rest-serves-the-body
  File "context.py", line 558, in s3_website_urls
  simparity.behavioral.context.CapabilityNotOffered: the emulator offers no S3 website endpoint (the run names no website endpoint template: s3_website_url=, SIMPARITY_S3_WEBSITE_URL)
Expected (AWS): a website-host GET and HEAD of `old.html` answer HTTP 301 with Location exactly `/new.html` and no object body; `ext.html` answers 301 with Location exactly `https://example.com/landing`; the REST URL for `old.html` answers 200 with the stored body, the header x-amz-website-redirect-location `/new.html`, and no Location header.
Actual (emulator): no website endpoint is published, so no website request is made and none of those responses exists.

The bucket names, queue names, per-run Referer secret and object bodies are per-run values and are not part of the requirement; the requirements are the delivery of a matching key, the 301/Location pair on the website endpoint and the 200/body/x-amz-website-redirect-location triple on REST.

## Steps to Reproduce
Cause 1:
1. Boot the emulator from this checkout. Create a bucket and an SQS queue whose policy allows s3.amazonaws.com sqs:SendMessage with aws:SourceArn equal to the bucket ARN.
2. PutBucketNotificationConfiguration with one QueueConfiguration: Events ['s3:ObjectCreated:*'], Filter.Key.FilterRules [{Name: 'prefix', Value: 'incoming/'}, {Name: 'suffix', Value: '.csv'}]. Drain the queue to burn the s3:TestEvent.
3. PutObject key `incoming/report.csv`, then drain the queue: on AWS one record arrives with that key. Observe that no record arrives.
4. PutObject `other/report.csv`, `incoming/report.json`, `other/report.json`, `report.csv`, `incoming/nested/report.csv.bak`, then drain again: on AWS none is delivered, and all six objects are listable.

Cause 2:
1. Create a bucket, lift BlockPublicPolicy/RestrictPublicBuckets and put a Referer-conditioned public-read bucket policy.
2. PutObject `old.html` with WebsiteRedirectLocation `/new.html`, `ext.html` with `https://example.com/landing`, plus `new.html` and `index.html`.
3. PutBucketWebsite with IndexDocument.Suffix `index.html`.
4. GET `http://<bucket>.s3-website-<region>.amazonaws.com/old.html` (and the `s3-website.<region>` form): on AWS 301 with Location `/new.html` and no body. Observe that the emulator publishes no such host at all, so no request can be addressed.
5. GET the REST URL for the same key: on AWS 200 with the body, x-amz-website-redirect-location `/new.html` and no Location header.
6. Re-replay the two checks in trace_ids to confirm a fix.
