Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.subscribe-to-shard-second-subscription-rejected
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.subscribe-to-shard-second-subscription-rejected|_read_until_fault#18|AssertionError|raise AssertionError(
Issue-Commit: cb16ceff5b6eb3896a41d4deca735a39546ae6bc

# kinesis: two overlapping SubscribeToShard subscriptions for one consumer+shard are never faulted in band — the incumbent stream just ends cleanly instead of receiving the ResourceInUseException event

> _Note: this issue was identified against the code base at commit `cb16ceff5b6eb3896a41d4deca735a39546ae6bc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubscribeToShard

## Description
One net-new root cause in the kinesis enhanced-fan-out (EFO) data plane, grounded in the failing behavioral check kinesis.subscribe-to-shard-second-subscription-rejected, which the replay report marks neither ALREADY FILED nor OWNED. Related but distinct open/fixed work: the open issue 20261114T051647Z-kinesis-3-net-new-causes-a-subscribetoshard-subscription.md lists this check under its Cause 1 ("the subscription ends immediately / pushes no periodic events"), and that symptom no longer reproduces — the solo control leg now streams, delivers both records with ContinuationSequenceNumber and MillisBehindLatest set, and keeps pushing events while idle, so the check now runs past that assertion. The fixed issue 20261114T051647Z-kinesis-subscribetoshard-refuses-the-documented-checkpoint-and-resubscribe.md removed the unconditional pre-flight ResourceInUseException refusal; the second SubscribeToShard is now correctly ACCEPTED with HTTP 200. What remains, and is reported here, is the behaviour that becomes observable once it is accepted: the conflict is never signalled on either event stream. This is NOT a refusal of an operation a fixed issue claimed to fix (the call succeeds), so it is reported as a new defect rather than a REOPENS.

=== Cause 1 of 1: a contested EFO subscription is terminated silently instead of being faulted in band with ResourceInUseException ===
AWS: Smithy: SubscribeToShard input { ConsumerARN (required), ShardId (required), StartingPosition (required), DryRun }, output { EventStream: SubscribeToShardEventStream, required, @streaming }. SubscribeToShardEventStream is "a tagged union for all of the types of events an enhanced fan-out consumer can receive over HTTP/2 after a call to SubscribeToShard", and its members are SubscribeToShardEvent, ResourceNotFoundException, ResourceInUseException, the KMS* family and InternalFailureException — i.e. ResourceInUseException ("The resource is not available for this operation", client error, HTTP 400 when returned as an operation error) is also deliverable as an IN-BAND EVENT of the stream. Recorded behavior: while one subscription of a given ConsumerARN+ShardId is live, a second SubscribeToShard for the same pair issued inside the documented 5-second window is ACCEPTED (HTTP 200 with a live vnd.amazon.eventstream body — not an HTTP error), and then BOTH subscriptions — the incumbent and the challenger — are terminated by an in-band ResourceInUseException event. Its message is "Another active subscription exists for this consumer: <accountId>:<streamName>:<shardId>:<consumerName>": the fixed text is "Another active subscription exists for this consumer: " and the variable part is the colon-joined caller account id, stream name, shard id and consumer name taken from the request and the emulator's state (never the literal ids of any one run). With both faulted streams closed, a fresh SubscribeToShard for the same consumer and shard is accepted again and replays from TRIM_HORIZON, so the fault is about concurrency, not a poisoned consumer.
Emulator: the second SubscribeToShard is accepted with 200 and a live stream (correct), but neither stream is ever faulted. The incumbent subscription ENDS CLEANLY after 5 SubscribeToShardEvents with no error event at all, so the check's read-until-fault loop runs out of frames: "AssertionError: the subscription ended cleanly after 5 events instead of faulting with the in-band conflict error". There is no in-band fault path for contention in the handler: its module docs describe contention as a pre-flight HTTP refusal returned from establish() (services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/subscribe_to_shard.rs lines 103-105 and 302-315), the subscription record is released "the moment that event stream ends" (lines 66-71), and the only non-data union member the framing code names is KMSDisabledException (KMS_DISABLED_EVENT, line 158) — ResourceInUseException is never framed as an event.
Why: an EFO consumer (the KCL's default read path) distinguishes "my subscription was contested/taken over, stop and re-acquire" from "the window ended normally, renew from my checkpoint" purely by the in-band ResourceInUseException. Against the emulator both look identical: a clean end of stream. Two workers that race onto the same consumer+shard are therefore both served, both read the shard and silently double-process every record, with no signal that the lease is contested; conversely code that asserts the conflict is reported (or that waits for the fault before re-acquiring) passes in AWS and hangs or duplicates here. The modelled ResourceInUseException member of SubscribeToShardEventStream is unreachable, and where the handler's pre-flight check does fire instead it answers an HTTP 400 for a case AWS answers 200 for.
Fix: the SubscribeToShard handler and its event-stream serialization in services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/subscribe_to_shard.rs (with the framing helpers in service_common::event_stream and the shapes in src/ops/types/subscribe_to_shard_types.rs), plus the per-consumer live-subscription bookkeeping it judges against in that crate's state module (ConsumerRecord::subscribed_at / subscription_ended): establishing a second subscription for a consumer+shard that already has a live one must be served, must mark the incumbent as contested, and both pushing loops must then emit the ResourceInUseException member of the output union as their final frame and close — rather than refusing at the HTTP layer or letting the streams end as if their window had simply elapsed. The slot must be released afterwards so a later SubscribeToShard is accepted again.
Traces: kinesis.subscribe-to-shard-second-subscription-rejected.

## Evidence
Check kinesis.subscribe-to-shard-second-subscription-rejected (mismatch):
  File "check_kinesis_subscribe_to_shard_second_subscription_rejected.py", line 391, in run
    code, message, _got, evs = _read_until_fault(stream)
  File "check_kinesis_subscribe_to_shard_second_subscription_rejected.py", line 266, in _read_until_fault
  AssertionError: the subscription ended cleanly after 5 events instead of faulting with the in-band conflict error

Sequence the check performed (all against the emulator):
  1. solo SubscribeToShard (TRIM_HORIZON) — PASSED: delivered b'efo-alpha', b'efo-beta' in order, every event carried a non-empty ContinuationSequenceNumber and an integer MillisBehindLatest >= 0, and 10 further seconds of reading produced events with no records and no fault.
  2. incumbent SubscribeToShard (TRIM_HORIZON) -> HTTP 200, live event stream.
  3. challenger SubscribeToShard for the SAME ConsumerARN + ShardId, issued < 5 s later -> HTTP 200, live event stream (accepted, as AWS does — no ClientError).
  4. read the incumbent expecting an in-band fault.

Expected (AWS): the incumbent stream's next frame is an in-band event of type ResourceInUseException, i.e. botocore raises EventStreamError with Error.Code == 'ResourceInUseException' and Error.Message == "Another active subscription exists for this consumer: <accountId>:<streamName>:<shardId>:<consumerName>"; the challenger stream is then faulted with the identical code and message. (The account id, stream name, shard id and consumer name are request/state values filling that template; only the quoted prefix is fixed text.)
Actual (emulator): the incumbent stream delivered 5 SubscribeToShardEvents and then terminated normally — no EventStreamError, no error event, no error code and no message. The challenger was never reached because the incumbent's assertion failed first; no fault was produced for it either.

## Steps to Reproduce
1. CreateStream(ShardCount=1, StreamModeDetails={'StreamMode':'PROVISIONED'}), wait for ACTIVE; read StreamARN via DescribeStreamSummary and the ShardId via ListShards.
2. RegisterStreamConsumer(StreamARN, ConsumerName) and wait for DescribeStreamConsumer to report ConsumerStatus ACTIVE.
3. PutRecord twice (e.g. b'efo-alpha' then b'efo-beta', PartitionKey='k').
4. SubscribeToShard(ConsumerARN, ShardId, StartingPosition={'Type':'TRIM_HORIZON'}) and keep the event stream open — this is the incumbent.
5. Immediately (within 5 s) call SubscribeToShard again with the SAME ConsumerARN and ShardId. Observe it is accepted with HTTP 200 and a live event stream (this part is correct).
6. Iterate the INCUMBENT event stream. Expected: it faults with an in-band ResourceInUseException event whose message is "Another active subscription exists for this consumer: <account>:<stream>:<shard>:<consumer>". Observed: it yields a handful of SubscribeToShardEvents and then ends cleanly with no error.
7. Iterate the CHALLENGER stream: it too must fault with the same code and message; it does not.
8. Close both streams and SubscribeToShard again from TRIM_HORIZON — it must be accepted and redeliver the oldest record, confirming the fault was about concurrency only.
