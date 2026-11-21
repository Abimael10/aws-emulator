Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache__sns.cache-cluster-event-notification-reaches-the-sns-subscriber
Issue-Key: elasticache__sns.cache-cluster-event-notification-reaches-the-sns-subscriber|_body#1|Timeout|time.sleep(_FAST_POLL_S)
Issue-Commit: bf168f2125883f6ad6bec616d1c212f3f21e9ddc

# elasticache__sns: a cache cluster's events are never published to its configured SNS topic (1 cause)

> _Note: this issue was identified against the code base at commit `bf168f2125883f6ad6bec616d1c212f3f21e9ddc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
One root cause on the implemented ElastiCache cache-cluster surface, exercised by the single surviving failing check in this replay, elasticache__sns.cache-cluster-event-notification-reaches-the-sns-subscriber.

Note on existing issues: open issues 20261109T054811Z-elasticache-sns-elasticache-cluster-events-are-never-published-to-the.md and 20261112T022229Z-elasticache-sns-elasticache-cluster-events-are-never-published-to-the-69d935.md describe this same cause and carry this same check id, but the replay report in this run marked this check neither ALREADY FILED nor OWNED, so it is re-filed as instructed. Whoever fixes any of the three should close them together; this is one work item, not three.

Cause 1 of 1: a cache cluster that carries a NotificationTopicArn publishes NOTHING to that SNS topic — cluster events reach only ElastiCache's own event log.
AWS: Smithy (com.amazonaws.elasticache, AmazonElastiCacheV9): CacheCluster$NotificationConfiguration is a NotificationConfiguration {TopicArn, TopicStatus} whose shape doc reads "Describes a notification topic and its status. Notification topics are used for publishing ElastiCache events to subscribers using Amazon Simple Notification Service (SNS)."; ModifyCacheCluster$NotificationTopicStatus — "The status of the Amazon SNS notification topic. Notifications are sent only if the status is active. Valid values: active | inactive"; CreateCacheCluster$NotificationTopicArn names the topic events are sent to. The delivery itself is recorded behavior only (ElastiCache ECEvents.SNS): while NotificationConfiguration.TopicStatus is 'active', every cluster event ElastiCache records for that cluster is ALSO published to the named topic, so an SQS subscriber of that topic receives an SNS envelope with Type='Notification' and TopicArn equal to the configured topic, whose Message carries the documented event name in the form `ElastiCache:<Event>` together with the cluster id (a node reboot yields the ElastiCache:CacheNodeReboot* events). Fixed parts of the requirement: the SNS envelope shape, Type='Notification', and the `ElastiCache:` prefix on the event name. Variable parts, taken from the request and from state: the topic ARN, the cluster id, the event name and the timestamps — never the particular ids of one recorded run.
Emulator: the configured topic ARN is stored and reported back (DescribeCacheClusters answers NotificationConfiguration with TopicStatus='active'), and DescribeEvents does record the reboot, but no message is ever published to the topic: the subscribed SQS queue stays at ApproximateNumberOfMessages 0 and the check's notification wait (600 s budget) never terminates, so the harness cuts the check at its 60 s ceiling.
Why: a client wiring cluster events to SNS (the documented ElastiCache -> SNS -> SQS/Lambda alerting pattern) is told TopicStatus='active' for its own topic and then never receives a message — no error, just silence. It never learns that a node rebooted, that provisioning finished, or that a failover happened, and an absence-based assertion for 'notifications inactive' passes for the wrong reason because there is no delivery path at all.
Fix: the elasticache cluster-event path — where a cluster event becomes real (the crate's event recording/settle logic in src/state.rs plus the create/modify/reboot/delete cache-cluster handlers that drive it) must publish each recorded cluster event to the cluster's configured topic whenever that topic's status is active, not merely append it to the local event log.
Traces: elasticache__sns.cache-cluster-event-notification-reaches-the-sns-subscriber — arm A reboots the single node (RebootCacheCluster with the node ids DescribeCacheClusters(ShowCacheNodeInfo=true) reports), asserts the reboot IS in DescribeEvents, then waits for a matching SNS notification on its own queue; that wait cannot terminate.

Harness caveat for re-replay: this check's own absence window has a 240 s floor (_ABSENCE_FLOOR_S) which is above the suite's 60 s per-check ceiling, so the check cannot report PASS inside that ceiling even after the publish path lands. Verify on the observables listed under Steps to Reproduce instead.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against this checkout: 138/194 passed (71.1%). Service elasticache__sns 1/2 (50%), 1 failed.

Failing check (marked neither ALREADY FILED nor OWNED in this report):
  [mismatch] elasticache__sns.cache-cluster-event-notification-reaches-the-sns-subscriber
  Error: Check timed out after 60.0s

Expected (AWS): after CreateCacheCluster(..., NotificationTopicArn=<topic>) settles to CacheClusterStatus='available' with NotificationConfiguration={TopicArn:<topic>, TopicStatus:'active'} and a RebootCacheCluster completes, an SQS subscriber of <topic> receives an SNS envelope with Type='Notification', TopicArn=<topic> and a Message containing `ElastiCache:<Event>` plus the cluster id; the queue's ApproximateNumberOfMessages becomes >= 1. Setting NotificationTopicStatus='inactive' suppresses delivery while DescribeEvents keeps recording; setting it back to 'active' resumes delivery.
Actual (emulator): DescribeEvents records the reboot event and NotificationConfiguration reports the configured topic as 'active', but nothing is ever delivered — the queue depth stays 0 and the check's _await_notification loop burns the whole per-check ceiling ("Check timed out after 60.0s").

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. sns CreateTopic; sqs CreateQueue; sqs SetQueueAttributes with a Policy allowing sns.amazonaws.com sqs:SendMessage on that queue for that topic ARN; sns Subscribe(Protocol='sqs', Endpoint=<queue arn>). Prove the pipe with a plain sns Publish that arrives on the queue.
3. ec2 CreateVpc + CreateSubnet (one per available AZ) + CreateSecurityGroup; elasticache CreateCacheSubnetGroup over those subnets.
4. elasticache CreateCacheCluster(CacheClusterId=<id>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, Port=6379, CacheSubnetGroupName=<group>, SecurityGroupIds=[<sg>], NotificationTopicArn=<topic>); poll DescribeCacheClusters until CacheClusterStatus='available' and NotificationConfiguration={TopicArn:<topic>, TopicStatus:'active'}.
5. elasticache RebootCacheCluster(CacheClusterId=<id>, CacheNodeIdsToReboot=[the node ids from DescribeCacheClusters(ShowCacheNodeInfo=true)]).
6. elasticache DescribeEvents(SourceIdentifier=<id>, SourceType='cache-cluster') — the reboot event IS recorded.
7. sqs ReceiveMessage and sqs GetQueueAttributes(AttributeNames=['ApproximateNumberOfMessages']) — expect at least one SNS Notification envelope whose TopicArn is <topic> and whose Message carries `ElastiCache:<Event>` and <id>; observe that nothing is ever delivered and the depth stays 0.
8. ModifyCacheCluster(CacheClusterId=<id>, ApplyImmediately=true, NotificationTopicStatus='inactive'), reboot again: DescribeEvents must still record the event and the queue must stay at 0. Then set it back to 'active', reboot again, and delivery must resume.
9. Re-replay elasticache__sns.cache-cluster-event-notification-reaches-the-sns-subscriber (see the harness caveat: its 240 s absence floor exceeds the suite's 60 s per-check ceiling, so confirm on the observables in steps 7-8).
