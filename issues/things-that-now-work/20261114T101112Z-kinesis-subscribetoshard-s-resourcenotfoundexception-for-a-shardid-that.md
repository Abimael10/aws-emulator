Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.subscribe-to-shard-with-shard-id-from-another-stream-rejected
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.subscribe-to-shard-with-shard-id-from-another-stream-rejected|run#31|AssertionError|assert error.get("Message") == want, f"expected message {want!r}, got {error.get('Message')!r}"
Issue-Commit: 06c8495fe56890fc6ce0ceaf480e6ed296e74be7

# kinesis: SubscribeToShard's ResourceNotFoundException for a ShardId that is not in the consumer's stream uses the wrong message wording

> _Note: this issue was identified against the code base at commit `06c8495fe56890fc6ce0ceaf480e6ed296e74be7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubscribeToShard

## Description
One net-new root cause in the kinesis enhanced-fan-out control path, grounded in a failing behavioral check that the replay report marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: the ResourceNotFoundException raised when ShardId names no shard of the consumer's stream carries a non-AWS message.

AWS: Smithy: SubscribeToShard input { ConsumerARN required, ShardId required, StartingPosition required }; modeled error ResourceNotFoundException, errorKind client, HTTP 400 ("The requested resource could not be found. The stream might not be specified correctly."). A ConsumerARN is its stream's ARN extended, so ShardId is resolved within THAT stream only; a shard id that exists only in another stream is refused. Recorded behavior for this refusal class: HTTP 400, error code 'ResourceNotFoundException', message "Shard <shardId> in stream <streamName> under account <accountId> does not exist". The fixed text of that message is "Shard " / " in stream " / " under account " / " does not exist", with no trailing period; its variable parts are the ShardId taken from the request, the name of the stream the ConsumerARN resolves to, and the 12-digit account id of the caller — all from the request and the emulator's own state, never literal values from one run.

Emulator: the status (400) and the error code (ResourceNotFoundException) are correct, but the message is a different sentence: 'Could not find shard shardId-000000000001 in stream sim2real-behav-kds-8d2482f329ab under account 000000000000.' — i.e. the frame "Could not find shard <shardId> in stream <streamName> under account <accountId>." (different leading clause, and a terminating period AWS does not emit) where AWS answers "Shard <shardId> in stream <streamName> under account <accountId> does not exist". The check's positive control passed moments earlier (the same consumer subscribing to a shard id its own stream does hold), the consumer stayed ACTIVE and the foreign stream's record was left unread, so the shard-resolution behaviour itself is right and only the message text diverges.

Why: the message is part of the contract for this error class. Clients, SDK retry/branching helpers and test suites that match on the refusal sentence (or that surface it to operators as the reason a subscription was refused) read a sentence AWS never emits, so a parse keyed on AWS's wording fails against the emulator and passes in AWS, and operator-facing diagnostics differ between the two.

Fix: the shard-resolution refusal branch of the SubscribeToShard handler, services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/subscribe_to_shard.rs (its documented ResourceNotFoundException branch — "the ShardId names no shard of it", lines 75-77) — the ResourceNotFoundException it builds for an unresolvable ShardId must be formatted with AWS's "Shard {shard_id} in stream {stream_name} under account {account_id} does not exist" frame, filled from the request's ShardId, the stream the ConsumerARN resolves to and the caller's account.

Traces: kinesis.subscribe-to-shard-with-shard-id-from-another-stream-rejected.

## Evidence
Cause 1 — wrong ResourceNotFoundException message (1 check):
  kinesis.subscribe-to-shard-with-shard-id-from-another-stream-rejected:
  AssertionError: expected message 'Shard shardId-000000000001 in stream sim2real-behav-kds-8d2482f329ab under account 000000000000 does not exist', got 'Could not find shard shardId-000000000001 in stream sim2real-behav-kds-8d2482f329ab under account 000000000000.'
  (assertion at check line 374: assert error.get("Message") == want)

  Expected: HTTP 400, Error.Code 'ResourceNotFoundException', Error.Message "Shard <ShardId from the request> in stream <stream the ConsumerARN resolves to> under account <caller account id> does not exist" (no trailing period).
  Actual:   HTTP 400, Error.Code 'ResourceNotFoundException' (correct), Error.Message "Could not find shard <ShardId> in stream <stream> under account <account>."
  The stream name, shard id and account id above are per-run/per-environment values that fill the template; the divergence is the sentence frame itself.
  Everything else the check asserts passed: the error code, the positive-control subscription on the consumer's own shard (which delivered that stream's record and not the other stream's), the consumer remaining ACTIVE with an unchanged ConsumerARN, and the foreign stream's record still readable via GetShardIterator/GetRecords.

Related but not covering this: open issues 20261114T051647Z-kinesis-3-net-new-causes-a-subscribetoshard-subscription.md and 20261114T051647Z-kinesis-subscribetoshard-refuses-the-documented-checkpoint-and-resubscribe.md concern the subscription window, authorization and ResourceInUseException-on-resubscribe — none of them carries this check or this message frame.

## Steps to Reproduce
1. CreateStream(StreamName=own, ShardCount=1) and CreateStream(StreamName=other, ShardCount=2); wait for both to be ACTIVE.
2. ListShards on both. `other` has a second shard id (shardId-000000000001) that `own`, being 1-shard, does not have.
3. DescribeStreamSummary(own) -> StreamARN; RegisterStreamConsumer(StreamARN=own_arn, ConsumerName=c); wait for the consumer to be ACTIVE.
4. PutRecord into `own` (PartitionKey='k'), and PutRecord into `other` with ExplicitHashKey = the foreign shard's HashKeyRange.StartingHashKey.
5. Positive control: SubscribeToShard(ConsumerARN=consumer_arn, ShardId=<own's only shard>, StartingPosition={'Type':'TRIM_HORIZON'}) — succeeds and delivers own's record.
6. Change only the shard id: SubscribeToShard(ConsumerARN=consumer_arn, ShardId='shardId-000000000001', StartingPosition={'Type':'TRIM_HORIZON'}).
   Observe HTTP 400 ResourceNotFoundException whose Message is "Could not find shard shardId-000000000001 in stream <own> under account <account>." instead of AWS's "Shard shardId-000000000001 in stream <own> under account <account> does not exist".
