Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.tag-apis-on-deregistered-consumer-arn-rejected kinesis.e2e-stream-consumer-fanout-lifecycle
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.tag-apis-on-deregistered-consumer-arn-rejected|run#19|AssertionError|assert message == expected_message, f"unexpected message {message!r}"
Issue-Key: kinesis.e2e-stream-consumer-fanout-lifecycle|run#28|Timeout|event = events_q.get(timeout=5)
Issue-Commit: 2569c6911135fdcd350a29db0e205ccfafae52a0

# kinesis: 2 net-new causes — the consumer-not-found refusal of the tag APIs interpolates a creation timestamp AWS does not emit, and an enhanced fan-out subscription never delivers records written after it is established

> _Note: this issue was identified against the code base at commit `2569c6911135fdcd350a29db0e205ccfafae52a0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Two net-new root causes in the kinesis service, both grounded in failing behavioral checks of this replay. Neither is marked ALREADY FILED or OWNED, and neither is covered by an open issue (the nearest ones are named per cause).

Cause 1 of 2: the ResourceNotFoundException the generic tag APIs raise for a deregistered stream-consumer ARN carries an extra " with creation timestamp <epoch>" clause that AWS's message does not contain. Failing check: kinesis.tag-apis-on-deregistered-consumer-arn-rejected.

AWS: Smithy (com-amazonaws-kinesis-kinesis-20131202) models ListTagsForResource, TagResource and UntagResource with ResourceARN @required @length 1..2048 @pattern ^arn:aws.*:kinesis:.*:\d{12}:.*(stream|channel)/\S+$ — a consumer ARN arn:<partition>:kinesis:<region>:<account>:stream/<stream-name>/consumer/<consumer-name>:<registration-epoch> satisfies it — and each models ResourceNotFoundException (HTTP 400). Recorded behavior for the message: an ARN naming a consumer that no longer exists is refused with Error.Code ResourceNotFoundException and Error.Message "Consumer <consumerName> under stream: <streamName>, account <accountId> not found." The fixed text is "Consumer ", " under stream: ", ", account " and " not found."; the only variable parts are the consumer name and stream name parsed out of the request's ResourceARN and the caller's account id. All three operations emit the same message for the same ARN. The positive control of the check shows the live path works: before DeregisterStreamConsumer, TagResource then ListTagsForResource on the consumer ARN succeed and return the tag.

Emulator: the code is right and the message is wrong — after DeregisterStreamConsumer (and the consumer no longer resolving through DescribeStreamConsumer) all three tag APIs answer ResourceNotFoundException with "Consumer <consumerName> with creation timestamp <registrationEpochSeconds> under stream: <streamName>, account <accountId> not found.", inserting the clause " with creation timestamp <epoch>" after the consumer name. Replay detail: "AssertionError: unexpected message 'Consumer <consumer> with creation timestamp <epoch> under stream: <stream>, account 000000000000 not found.'" (the consumer name, epoch, stream name and account id are per-run values filling the template; the divergence is the extra clause, not those values).

Why: the refusal message is part of the contract clients match on. Deregistration and cleanup flows that key on this message (test assertions, log-based alerting, retry/idempotency logic that treats "already gone" as success) do not recognise the emulator's variant, and the injected registration epoch leaks an internal timestamp into a message AWS keys only on names.

Fix: the shared refusal-message builder in the kinesis service state, crate::state::resource_not_found_message, used by the tag family — services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/list_tags_for_resource.rs lines 192-208 and the matching arms of the TagResource and UntagResource handlers — must render a consumer ARN's not-found message without the creation-timestamp clause. Related open issue that does NOT cover this: 20261117T225913Z-kinesis-2-net-new-causes-stopstreamencryption-never-validates.md quotes the same emulator string, but its Cause 2 is that a LIVE consumer ARN fails to resolve at all (replay keys kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected and kinesis.consumer-resource-tags-lifecycle-independent-of-stream-tags); here the live consumer resolves and only the genuinely-absent consumer's wording diverges.

Cause 2 of 2: an enhanced fan-out subscription delivers its initial confirmation event and then nothing — records written to the shard while the subscription is open are never pushed, so the full consumer-fanout lifecycle never completes. Failing check: kinesis.e2e-stream-consumer-fanout-lifecycle.

AWS: Smithy models SubscribeToShard with required ConsumerARN/ShardId/StartingPosition and a single required output member EventStream targeting SubscribeToShardEventStream marked @streaming, i.e. a long-lived application/vnd.amazon.eventstream response. Recorded behavior: on a fresh subscription the first SubscribeToShardEvent carries no Records (the stream has not been written to); the subscription is then held open and every record a subsequent PutRecords puts on that shard arrives in a following SubscribeToShardEvent within seconds (order across a PutRecords batch is not guaranteed), after which DeregisterStreamConsumer and DeleteStream complete the lifecycle.

Emulator: the check exhausts the harness budget — "Error: Check timed out after 60.0s". The initial confirmation event arrives, PutRecords of three payloads is accepted, and the 60 s wait for those three records on the live subscription never observes them, so the trailing DeregisterStreamConsumer / DeleteStream legs are never reached. Hypothesis: the subscription is completed right after the first event instead of being held open for its window, so writes that follow the subscribe are never pushed — the same shape open issue 20261114T051647Z-kinesis-3-net-new-causes-a-subscribetoshard-subscription.md reports for its own checks ("a SubscribeToShard subscription ends immediately instead of being held open"); that issue does not carry this check as a replay key, and this check additionally grades delivery of post-subscribe writes and the deregister/delete teardown.

Why: enhanced fan-out is the KCL 2.x default read path. A subscription that only ever reports the backlog snapshot and never pushes new records makes every EFO consumer appear healthy while silently processing nothing, and a client blocks on the event stream until its own timeout rather than receiving data or an error.

Fix: the SubscribeToShard handler's subscription loop in the kinesis service — the event-stream response must stay open for the modeled subscription window and keep emitting SubscribeToShardEvents (including record-less heartbeats) as the shard's record store receives new puts, instead of terminating after the first event.

Traces: kinesis.tag-apis-on-deregistered-consumer-arn-rejected (Cause 1), kinesis.e2e-stream-consumer-fanout-lifecycle (Cause 2).

## Evidence
Cause 1 — tag APIs on a deregistered consumer ARN:
[mismatch] kinesis.tag-apis-on-deregistered-consumer-arn-rejected
  File "check_kinesis_tag_apis_on_deregistered_consumer_arn_rejected.py", line 117, in run
    assert message == expected_message, f"unexpected message {message!r}"
  AssertionError: unexpected message 'Consumer sim2real-consumer-<id> with creation timestamp <epoch> under stream: sim2real-behav-kds-<id>, account 000000000000 not found.'
Expected (AWS): HTTP 400, Error.Code ResourceNotFoundException, Error.Message "Consumer <consumerName> under stream: <streamName>, account <accountId> not found." from ListTagsForResource, TagResource AND UntagResource on the deregistered consumer's ARN.
Actual (emulator): the same code and status, but the message inserts " with creation timestamp <registrationEpochSeconds>" after the consumer name. The per-run consumer name, stream name, epoch and account id are template values; the extra clause is the divergence.
The check's positive control (TagResource + ListTagsForResource on the consumer while ACTIVE) succeeded, so only the absent-consumer wording diverges.

Cause 2 — enhanced fan-out lifecycle:
[mismatch] kinesis.e2e-stream-consumer-fanout-lifecycle
  Error: Check timed out after 60.0s
Expected (AWS): CreateStream(ShardCount=1) -> ACTIVE, RegisterStreamConsumer -> ACTIVE, SubscribeToShard(TRIM_HORIZON) yields a first SubscribeToShardEvent with Records == [], then PutRecords of b"fanout-record-0".. b"fanout-record-2" are delivered on that same open subscription (as one or more SubscribeToShardEvents, any order) within seconds, after which DeregisterStreamConsumer and DeleteStream are observed to take effect.
Actual (emulator): the confirmation event arrives, PutRecords is accepted, and the subscription yields no further SubscribeToShardEvent carrying those records for the whole 60 s wait, so the check is killed by the suite budget before the teardown legs run.

## Steps to Reproduce
Cause 1 (kinesis.tag-apis-on-deregistered-consumer-arn-rejected):
1. kinesis.CreateStream(StreamName=<s>, ShardCount=1); poll DescribeStreamSummary until ACTIVE; arn = StreamDescriptionSummary.StreamARN.
2. consumer_arn = kinesis.RegisterStreamConsumer(StreamARN=arn, ConsumerName=<c>).Consumer.ConsumerARN; poll DescribeStreamConsumer until ConsumerStatus==ACTIVE.
3. Positive control: kinesis.TagResource(ResourceARN=consumer_arn, Tags={"env":"canary"}) then ListTagsForResource(ResourceARN=consumer_arn) returns that tag.
4. kinesis.DeregisterStreamConsumer(ConsumerARN=consumer_arn); poll until DescribeStreamConsumer answers ResourceNotFoundException.
5. Call ListTagsForResource(ResourceARN=consumer_arn), TagResource(ResourceARN=consumer_arn, Tags={"env":"canary"}) and UntagResource(ResourceARN=consumer_arn, TagKeys=["env"]).
   -> emulator: ResourceNotFoundException "Consumer <c> with creation timestamp <epoch> under stream: <s>, account <acct> not found."
   -> AWS: ResourceNotFoundException "Consumer <c> under stream: <s>, account <acct> not found."

Cause 2 (kinesis.e2e-stream-consumer-fanout-lifecycle):
1. kinesis.CreateStream(StreamName=<s>, ShardCount=1); AddTagsToStream; wait ACTIVE; take StreamARN and the single ShardId from ListShards.
2. consumer_arn = RegisterStreamConsumer(StreamARN=..., ConsumerName=<c>); TagResource on consumer_arn; wait ConsumerStatus==ACTIVE.
3. In a background thread call SubscribeToShard(ConsumerARN=consumer_arn, ShardId=<shard>, StartingPosition={Type: TRIM_HORIZON}) and iterate the EventStream; assert the first event is a SubscribeToShardEvent with no Records.
4. kinesis.PutRecords with three records (pk-0..pk-2) on that stream and keep reading the subscription for up to 60 s.
   -> emulator: no further SubscribeToShardEvent carrying those records; the check hits the suite's 60 s budget.
   -> AWS: the three records arrive on the open subscription within seconds.
5. DeregisterStreamConsumer, wait for the consumer to disappear, DeleteStream, wait for the stream to disappear.
Replay: kinesis.tag-apis-on-deregistered-consumer-arn-rejected and kinesis.e2e-stream-consumer-fanout-lifecycle.
