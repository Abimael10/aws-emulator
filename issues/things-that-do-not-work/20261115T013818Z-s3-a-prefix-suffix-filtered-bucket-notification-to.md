Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.notification-filter-non-matching-key-not-delivered
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.notification-filter-non-matching-key-not-delivered|_drain_records#9|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 4f3c099690bf1a1a45af47b8a28528c58ba9b1de

# s3: a prefix+suffix-filtered bucket notification to SQS reaches no verdict — the matching key's ObjectCreated record is never observed, so the filter's negative half cannot be established

> _Note: this issue was identified against the code base at commit `4f3c099690bf1a1a45af47b8a28528c58ba9b1de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
One net-new cause (Cause 1 of 1), grounded in the failing check s3.notification-filter-non-matching-key-not-delivered, which the replay report lists under `### s3` and marks neither OWNED nor ALREADY FILED.

Cause 1 of 1: a bucket notification configured with a QueueConfiguration carrying BOTH a `prefix` and a `suffix` FilterRule delivers nothing a client can observe, so neither the positive half (the one key satisfying both rules is delivered) nor the negative half (keys breaking either rule are written but withheld) of the filter contract is reachable.

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model) models QueueConfiguration{Id, QueueArn (required, xmlName `Queue`), Events (required), Filter}, and NotificationConfigurationFilter -> S3KeyFilter{FilterRules} -> FilterRule{Name: FilterRuleName enum with exactly the values `prefix` and `suffix`, Value}. The delivery semantics themselves are not in the model, so this part is AWS documented behavior plus the recording: S3 publishes one record per matching event to the queue, applying prefix AND suffix conjunctively to the object key, and sends an `s3:TestEvent` once when the configuration is put (https://docs.aws.amazon.com/AmazonS3/latest/userguide/notification-how-to-filtering.html). A key matching both rules produces an s3:ObjectCreated:* record whose `s3.object.key` is that key; keys breaking either rule produce NO record while still being created as real objects — suppression is a routing decision, not a write failure; and the configuration keeps working after the suppressed puts.

Emulator: the check never reaches a verdict. It puts the configuration, burns a 20 s window to drain the s3:TestEvent, puts `incoming/report.csv` and then long-polls the queue for 60 s for that record; the run is killed by the 60 s harness cap with no delivery and no assertion outcome. No HTTP status or error code can be quoted: PutBucketNotificationConfiguration and PutObject are accepted, and the divergence is the absence of the delivered record.

Why: a client that wires S3 -> SQS with key filters — the standard ingestion pattern — sees an accepted configuration and real objects but no events, with nothing to distinguish "filtered out" from "never delivered". Worse, the positive control is the gate for the negative half: with no delivery at all, a filter that leaks every key and a filter that works are indistinguishable, so the whole prefix/suffix contract is untestable.

Fix: the S3 bucket-notification event pipeline — the ObjectCreated event emission and its SQS destination delivery — and the key-filter evaluation that decides which configurations a record is published to. The configuration state and the SQS queue policy are not implicated: the queue is created and given an s3.amazonaws.com sqs:SendMessage policy scoped to the bucket ARN, and the Put is accepted.

Related open issues that do NOT carry this check's replay key (named so a fixer does not assume they do): 20261113T151530Z-events-s3-sqs-per-event-type-s3-queue.md (per-event-type queue routing never settles), 20261112T091340Z-s3-sqs-a-versioned-bucket-s-objectremoved-deletemarkercreated.md (ObjectRemoved records never reach their queues), 20261029T185330Z-s3-putbucketnotificationconfiguration-accepts-an-sqs-destination-whose-queue.md and 20261102T111455Z-s3-bucket-notification-configuration-validation-and-projection-diverge.md (configuration-time validation/projection). None of them covers the prefix+suffix filtered delivery path this check exercises.

What the timeout does and does not establish: the harness cap is 60 s while this check's own observation windows (a 20 s TestEvent drain, then a 60 s wait for the matching record) already exceed it, so the cap alone does NOT prove the record would never arrive given longer — it establishes that no delivery is observable inside the budget the suite replays with, which is also what every sibling S3-notification delivery issue above records. Re-replay this id once S3 -> SQS delivery is in place.

Traces: s3.notification-filter-non-matching-key-not-delivered.

## Evidence
Suite simparity-behavioral-aws-v1, service group s3 835/930 passed. Failing check s3.notification-filter-non-matching-key-not-delivered [mismatch], unmarked in the report, fails with `Error: Check timed out after 60.0s`.

Expected (AWS): after put_bucket_notification_configuration with QueueConfigurations=[{Id:'only-incoming-csv', QueueArn:<queue>, Events:['s3:ObjectCreated:*'], Filter:{Key:{FilterRules:[{Name:'prefix',Value:'incoming/'},{Name:'suffix',Value:'.csv'}]}}}], a put of `incoming/report.csv` yields an SQS message whose Records[0].s3.object.key is `incoming/report.csv`; puts of `other/report.csv`, `incoming/report.json`, `other/report.json`, `report.csv` and `incoming/nested/report.csv.bak` yield NO records while all six keys are listed by ListObjectsV2; a later put of `incoming/second.csv` is delivered again.

Actual (emulator): no record is received. The check is killed at 60.0 s inside the first 60 s receive_message loop that waits for the matching key, so its own assertion (`match_key in delivered`, whose message is "matching key 'incoming/report.csv' was not delivered within 60s") never even reports. PutBucketNotificationConfiguration, CreateQueue, SetQueueAttributes and PutObject all succeeded, so the divergence is confined to event emission/delivery, not to the control-plane calls.

Per-run values (bucket name, queue name/URL/ARN, account id) fill the template; the requirement is the conjunctive prefix+suffix filtering and the delivery of one s3:ObjectCreated:* record per matching key, with the suppressed keys still present as objects.

## Steps to Reproduce
Replay the behavioral suite and re-run the check id in trace_ids.

Manually:
1. Create a bucket B and an SQS queue Q; set Q's policy to Allow Principal Service s3.amazonaws.com, Action sqs:SendMessage, Resource <Q arn>, Condition ArnEquals aws:SourceArn arn:aws:s3:::B.
2. s3.put_bucket_notification_configuration(Bucket=B, NotificationConfiguration={'QueueConfigurations':[{'Id':'only-incoming-csv','QueueArn':<Q arn>,'Events':['s3:ObjectCreated:*'],'Filter':{'Key':{'FilterRules':[{'Name':'prefix','Value':'incoming/'},{'Name':'suffix','Value':'.csv'}]}}}]}). Drain the one s3:TestEvent.
3. put_object(Bucket=B, Key='incoming/report.csv', Body=b'a,b\n1,2\n') and long-poll Q. Expect exactly one record with s3.object.key == 'incoming/report.csv'. Observed: nothing arrives.
4. put_object the five non-matching keys ('other/report.csv', 'incoming/report.json', 'other/report.json', 'report.csv', 'incoming/nested/report.csv.bak') and long-poll Q. Expect no records, and ListObjectsV2 to list all six keys.
5. put_object('incoming/second.csv') and long-poll Q. Expect one more record, proving the configuration is not left broken.
