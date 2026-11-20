Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.notification-filter-non-matching-key-not-delivered
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.notification-filter-non-matching-key-not-delivered|_drain_records#9|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: abb164e6ce3e65cba242bbd0cd4b0598950b9faf

# s3: the prefix+suffix notification filter check reaches no verdict — the whole 60 s budget is consumed before a key matching both FilterRules is delivered to its SQS queue

> _Note: this issue was identified against the code base at commit `abb164e6ce3e65cba242bbd0cd4b0598950b9faf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Cause 1 of 1: an s3:ObjectCreated:* QueueConfiguration carrying both a `prefix` and a `suffix` FilterRule produces no observable delivery, so the check that proves the filter admits exactly the keys matching both rules — and suppresses routing without failing the write — is cut off at the suite's 60 s budget before its first (positive-control) assertion.

AWS: Smithy — PutBucketNotificationConfiguration's QueueConfiguration has required QueueArn (xmlName `Queue`) and Events (xmlName `Event`, flattened) plus an optional Filter (NotificationConfigurationFilter -> S3KeyFilter -> FilterRule{Name: FilterRuleName enum `prefix`|`suffix`, Value}). FilterRule's own documentation states it identifies the objects to which the filtering rule applies and that overlapping prefixes/suffixes are not supported. The behavior AWS gives: a put of a key satisfying BOTH rules is delivered to the queue as an s3:ObjectCreated record whose `s3.object.key` is that key; a put of a key breaking either rule is NOT delivered; and in both cases the object IS created — suppression is a routing decision, never a write failure, so every suppressed key is still returned by ListObjectsV2. Configuring the notification also emits one `s3:TestEvent` to the queue, which is not an object record.

Emulator: the check never reaches any assertion. The replay reports only `Check timed out after 60.0s`. Its first wait after configuring the filter and burning the s3:TestEvent is a 60 s drain of the queue demanding the record for `incoming/report.csv` (the single key satisfying both the `incoming/` prefix and the `.csv` suffix); the emulator did not produce it inside the suite's budget, so neither the positive control, nor the negative half (five keys each breaking exactly one filter component must deliver nothing), nor the effect-present half (all six keys must still be listable), nor the "filter still works afterwards" beat is exercised.

Hypothesis: the filtered QueueConfiguration delivers nothing at all rather than over- or under-matching — the replay does not say which, because the positive control is the first thing it waits on.

Why: a client that relies on prefix/suffix-filtered S3 notifications gets a silent queue from the emulator: the pipeline looks configured (PutBucketNotificationConfiguration is accepted) but no ObjectCreated record ever arrives for a matching key, so neither the matching nor the suppressing half of the filter contract can be relied on, and a filter bug is indistinguishable from a filter that routes nothing.

Fix: the S3 event-notification dispatcher and its key-filter evaluation — match a created object's key against the configuration's prefix/suffix FilterRules and publish the s3:ObjectCreated record to the configured QueueArn for a match, while leaving the object written for a non-match. Related open issues cover other notification areas under different headings and do not carry this check's replay key: 20261117T142140Z-s3-sqs-the-over-100-notification-configuration-refusal.md (a URL-encoded prefix filter matched literally), 20261113T151530Z-events-s3-sqs-per-event-type-s3-queue.md (per-event-type routing), 20261028T220212Z-s3-sqs-putbucketnotificationconfiguration-accepts-an-sqs-destination-it.md (destination validation).

Traces: s3.notification-filter-non-matching-key-not-delivered.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

[mismatch] s3.notification-filter-non-matching-key-not-delivered
  Error: Check timed out after 60.0s

Expected (AWS): with a QueueConfiguration for s3:ObjectCreated:* and FilterRules prefix='incoming/' and suffix='.csv', a PutObject of `incoming/report.csv` yields one queue message whose Records[0].s3.object.key is `incoming/report.csv`; PutObject of `other/report.csv`, `incoming/report.json`, `other/report.json`, `report.csv` and `incoming/nested/report.csv.bak` yields no records; ListObjectsV2 still returns all six keys; and a later `incoming/second.csv` is delivered.
Actual (emulator): the check reaches none of those assertions — the positive-control drain for `incoming/report.csv` is still empty when the suite's 60 s budget expires, so no verdict is produced.

The bucket and queue names are per-run values generated by the check; the key names above are the check's fixed inputs and the prefix/suffix values are the requirement's variable parts taken from the request.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CreateBucket, CreateQueue, and set a queue policy allowing s3.amazonaws.com sqs:SendMessage with aws:SourceArn equal to the bucket ARN.
3. PutBucketNotificationConfiguration with one QueueConfiguration: Events=['s3:ObjectCreated:*'], QueueArn=<queue>, Filter.Key.FilterRules=[{prefix,'incoming/'},{suffix,'.csv'}]. Drain and discard the s3:TestEvent.
4. PutObject Key='incoming/report.csv' and ReceiveMessage from the queue: on AWS one ObjectCreated record arrives naming that key. Observe that no record arrives.
5. PutObject the five non-matching keys and ReceiveMessage: no records must arrive; then ListObjectsV2 — all six keys must be present.
6. Re-replay the check in trace_ids to confirm a fix.
