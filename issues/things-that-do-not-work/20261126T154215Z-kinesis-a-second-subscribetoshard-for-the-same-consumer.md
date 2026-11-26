Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.subscribe-to-shard-second-subscription-rejected
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.subscribe-to-shard-second-subscription-rejected|_read_until_fault#18|AssertionError|raise AssertionError(
Issue-Commit: ac0d009460b15386fb7f532991bd4fb2bd9b5be8

# kinesis: a second SubscribeToShard for the same consumer and shard is accepted but neither subscription is faulted in band — both event streams just end cleanly instead of carrying a ResourceInUseException

> _Note: this issue was identified against the code base at commit `ac0d009460b15386fb7f532991bd4fb2bd9b5be8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubscribeToShard

## Description
Cause 1 of 1: the one-subscription-per-consumer-per-shard contract of enhanced fan-out is not enforced in band. The emulator now accepts the second SubscribeToShard call with HTTP 200 and a live event stream (which is correct), but when the conflicting pair exists it terminates neither subscription with the in-band error: the incumbent's event stream simply reaches end-of-stream after a handful of SubscribeToShardEvents, so botocore raises no EventStreamError at all and the conflict is invisible.

AWS: Smithy: SubscribeToShard output { EventStream: SubscribeToShardEventStream, required, @streaming } and the operation models ResourceInUseException (HTTP 400, "The resource is not available for this operation. For successful operation, the resource must be in the ACTIVE state."). The operation's own documentation states the rule: "If you call SubscribeToShard again with the same ConsumerARN and ShardId within 5 seconds of a successful call, you'll get a ResourceInUseException. If you call SubscribeToShard 5 seconds or more after a successful call, the second call takes over the subscription and the previous connection expires or fails with a ResourceInUseException." Recorded behavior: the duplicate call is answered 200 with a live stream and the conflict is reported IN BAND on the event streams as a ResourceInUseException frame, which botocore surfaces as an EventStreamError with Error.Code 'ResourceInUseException'. Its message frame is the fixed text "Another active subscription exists for this consumer: " followed by the colon-joined identity of the contested subscription — account id, stream name, shard id and consumer name — all four taken from the request and the emulator's state (never from one recording: the account, generated stream name, shard id and consumer name are per-run values that fill the template). After both streams are closed, a fresh SubscribeToShard from TRIM_HORIZON must succeed again and replay the oldest record, i.e. the fault is about concurrency only.

Emulator: the control leg passes (a solo subscription delivers both records in order with ContinuationSequenceNumber and MillisBehindLatest set, and keeps pushing events for 10 s), and the duplicate call is accepted within the 5-second window. Reading the incumbent stream then ends cleanly after 5 events: "AssertionError: the subscription ended cleanly after 5 events instead of faulting with the in-band conflict error" (check_kinesis_subscribe_to_shard_second_subscription_rejected.py, _read_until_fault). No EventStreamError, no ResourceInUseException code, and no conflict message are ever produced for either the incumbent or the challenger. Hypothesis: the handler tracks no per-(consumer, shard) subscription slot at all, so a second subscription neither displaces nor is displaced by the first, and the stream is simply closed when its own window/record budget is exhausted.

Why: enhanced fan-out consumers (the KCL's default path) rely on this in-band fault to detect that another worker has taken over a shard lease and to stop reading it. Against the emulator two workers can hold the same consumer-shard pair indefinitely, each reading the same records, and the client never sees the ResourceInUseException it is written to handle — so duplicate-processing and lease-stealing logic that AWS exercises is dead code, and code that waits for the conflict to resolve ownership hangs or double-processes.

Fix: the SubscribeToShard handler's subscription bookkeeping and its event-stream termination path (the kinesis service crate's src/ops/subscribe_to_shard.rs plus the event-stream framing it emits through) — register the live subscription per (ConsumerARN, ShardId), detect a second one, and terminate BOTH streams with an in-band ResourceInUseException frame carrying the message frame above, releasing the slot so a later SubscribeToShard succeeds again.

Note on existing issues: open issue 20261114T051647Z-kinesis-3-net-new-causes-a-subscribetoshard-subscription.md lists this check id, but its recorded replay key for it is the earlier `assert quiet_events` stop (the subscription ending immediately), which no longer reproduces — the subscription now holds open and delivers, and the suite does not report this failure as owned. The divergence filed here is the next, distinct behaviour: the missing in-band conflict error.

Traces: kinesis.subscribe-to-shard-second-subscription-rejected.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

[mismatch] kinesis.subscribe-to-shard-second-subscription-rejected
  File "check_kinesis_subscribe_to_shard_second_subscription_rejected.py", line 391, in run
    code, message, _got, evs = _read_until_fault(stream)
  File "check_kinesis_subscribe_to_shard_second_subscription_rejected.py", line 266, in _read_until_fault
    raise AssertionError(
  AssertionError: the subscription ended cleanly after 5 events instead of faulting with the in-band conflict error

Expected (AWS): with two SubscribeToShard subscriptions open for the same ConsumerARN and ShardId (the second issued inside the documented 5-second window and answered HTTP 200), BOTH event streams fault in band with Error.Code 'ResourceInUseException' and Error.Message "Another active subscription exists for this consumer: <accountId>:<streamName>:<shardId>:<consumerName>" — fixed text "Another active subscription exists for this consumer: ", variable parts the caller's account id and the stream name, shard id and consumer name of the contested subscription. After both streams are closed, SubscribeToShard from TRIM_HORIZON succeeds and delivers the oldest record first.
Actual (emulator): the incumbent event stream reaches end-of-stream after 5 SubscribeToShardEvents with no error frame; botocore raises no EventStreamError, so no code and no message exist to compare. The challenger is never read because the assertion fires on the incumbent.

The generated stream name, shard id, consumer name and account id in the expected message are per-run values filling the template, not part of the requirement; the requirement is the in-band ResourceInUseException and its message frame.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CreateStream(ShardCount=1, StreamMode=PROVISIONED), wait ACTIVE, read StreamARN and the single ShardId.
3. RegisterStreamConsumer and wait for ConsumerStatus ACTIVE.
4. PutRecord two payloads under one partition key.
5. SubscribeToShard(ConsumerARN, ShardId, StartingPosition={'Type':'TRIM_HORIZON'}); read until both records arrive, confirm continued reading for 10 s does not fault, then close the stream (this control leg passes today).
6. SubscribeToShard again for the SAME ConsumerARN and ShardId (the incumbent), then immediately — within 5 seconds — issue a THIRD SubscribeToShard for the same pair (the challenger). Both are answered HTTP 200.
7. Iterate the incumbent's EventStream: on AWS it faults with an in-band ResourceInUseException naming the contested account:stream:shard:consumer; observe instead that it ends cleanly after a few events with no error. Iterate the challenger's stream: same absence.
8. Close both streams and SubscribeToShard from TRIM_HORIZON again: it must succeed and deliver the oldest record first.
9. Re-replay the check in trace_ids to confirm a fix.
